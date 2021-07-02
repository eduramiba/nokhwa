use crate::{
    mjpeg_to_rgb888, yuyv422_to_rgb888, CameraFormat, CameraInfo, CaptureAPIBackend,
    CaptureBackendTrait, FrameFormat, NokhwaError, Resolution,
};
use flume::Receiver;
use glib::Quark;
use gstreamer::{
    element_error,
    glib::Cast,
    prelude::{DeviceExt, DeviceMonitorExt, DeviceMonitorExtManual, ElementExt, GstBinExt},
    Bin, Caps, ClockTime, DeviceMonitor, Element, FlowError, FlowSuccess, MessageView,
    ResourceError, State,
};
use gstreamer_app::{AppSink, AppSinkCallbacks};
use gstreamer_video::{VideoFormat, VideoInfo};
use image::{ImageBuffer, Rgb};
use regex::Regex;
use std::{collections::HashMap, str::FromStr};

type PipelineGenRet = (Element, AppSink, Receiver<ImageBuffer<Rgb<u8>, Vec<u8>>>);

/// The backend struct that interfaces with `GStreamer`.
/// To see what this does, please see [`CaptureBackendTrait`].
/// # Quirks
/// - `Drop`-ing this may cause a `panic`.
pub struct GStreamerCaptureDevice {
    pipeline: Element,
    app_sink: AppSink,
    camera_format: CameraFormat,
    camera_info: CameraInfo,
    receiver: Receiver<ImageBuffer<Rgb<u8>, Vec<u8>>>,
    caps: Option<Caps>,
}

impl GStreamerCaptureDevice {
    /// Creates a new capture device using the `GStreamer` backend. Indexes are gives to devices by the OS, and usually numbered by order of discovery.
    ///
    /// `GStreamer` uses `v4l2src` on linux, `ksvideosrc` on windows, and `autovideosrc` on mac.
    ///
    /// If `camera_format` is `None`, it will be spawned with with 640x480@15 FPS, MJPEG [`CameraFormat`] default.
    /// # Errors
    /// This function will error if the camera is currently busy or if `GStreamer` can't read device information.
    pub fn new(index: usize, cam_fmt: Option<CameraFormat>) -> Result<Self, NokhwaError> {
        let camera_format = match cam_fmt {
            Some(fmt) => fmt,
            None => CameraFormat::default(),
        };

        if let Err(why) = gstreamer::init() {
            return Err(NokhwaError::CouldntOpenDevice(format!(
                "Failed to initialize GStreamer: {}",
                why.to_string()
            )));
        }

        let (camera_info, caps) = {
            let device_monitor = DeviceMonitor::new();
            let video_caps = match Caps::from_str("video/x-raw") {
                Ok(cap) => cap,
                Err(why) => {
                    return Err(NokhwaError::GeneralError(format!(
                        "Failed to generate caps: {}",
                        why.to_string()
                    )))
                }
            };
            let _video_filter_id = match device_monitor
                .add_filter(Some("Video/Source"), Some(&video_caps))
            {
                Some(id) => id,
                None => return Err(NokhwaError::CouldntOpenDevice(
                    "Failed to generate Device Monitor Filter ID with video/x-raw and Video/Source"
                        .to_string(),
                )),
            };
            if let Err(why) = device_monitor.start() {
                return Err(NokhwaError::CouldntOpenDevice(format!(
                    "Failed to start device monitor: {}",
                    why.to_string()
                )));
            }
            let device = match device_monitor.devices().get(index) {
                Some(dev) => dev.clone(),
                None => {
                    return Err(NokhwaError::CouldntOpenDevice(format!(
                        "Failed to find device at index {}",
                        index
                    )))
                }
            };
            device_monitor.stop();
            let caps = device.caps();
            (
                CameraInfo::new(
                    DeviceExt::display_name(&device).to_string(),
                    DeviceExt::device_class(&device).to_string(),
                    "".to_string(),
                    index,
                ),
                caps,
            )
        };

        let (pipeline, app_sink, receiver) = generate_pipeline(camera_format, index)?;

        Ok(GStreamerCaptureDevice {
            pipeline,
            app_sink,
            camera_format,
            camera_info,
            receiver,
            caps,
        })
    }

    /// Creates a new capture device using the `GStreamer` backend. Indexes are gives to devices by the OS, and usually numbered by order of discovery.
    ///
    /// `GStreamer` uses `v4l2src` on linux, `ksvideosrc` on windows, and `autovideosrc` on mac.
    /// # Errors
    /// This function will error if the camera is currently busy or if `GStreamer` can't read device information.
    pub fn new_with(index: usize, width: u32, height: u32, fps: u32) -> Result<Self, NokhwaError> {
        let cam_fmt = CameraFormat::new(Resolution::new(width, height), FrameFormat::MJPEG, fps);
        GStreamerCaptureDevice::new(index, Some(cam_fmt))
    }
}

impl CaptureBackendTrait for GStreamerCaptureDevice {
    fn camera_info(&self) -> CameraInfo {
        self.camera_info.clone()
    }

    fn camera_format(&self) -> CameraFormat {
        self.camera_format
    }

    fn set_camera_format(&mut self, new_fmt: CameraFormat) -> Result<(), NokhwaError> {
        let mut reopen = false;
        if self.is_stream_open() {
            self.stop_stream()?;
            reopen = true;
        }
        let (pipeline, app_sink, receiver) = generate_pipeline(new_fmt, *self.camera_info.index())?;
        self.pipeline = pipeline;
        self.app_sink = app_sink;
        self.receiver = receiver;
        if reopen {
            self.open_stream()?;
        }
        self.camera_format = new_fmt;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_sign_loss)]
    fn compatible_list_by_resolution(
        &self,
        fourcc: FrameFormat,
    ) -> Result<HashMap<Resolution, Vec<u32>>, NokhwaError> {
        let mut resolution_map = HashMap::new();

        let frame_regex = Regex::new(r"(\d+/1)|((\d+/\d)+(\d/1)*)").unwrap();

        match self.caps.clone() {
            Some(c) => {
                for capability in c.iter() {
                    match fourcc {
                        FrameFormat::MJPEG => {
                            if capability.name() == "image/jpeg" {
                                let mut fps_vec = vec![];

                                let width = match capability.get::<i32>("width") {
                                    Ok(w) => w,
                                    Err(why) => {
                                        return Err(NokhwaError::CouldntQueryDevice {
                                            property: "Capibilities by Resolution: Width"
                                                .to_string(),
                                            error: why.to_string(),
                                        })
                                    }
                                };
                                let height = match capability.get::<i32>("height") {
                                    Ok(w) => w,
                                    Err(why) => {
                                        return Err(NokhwaError::CouldntQueryDevice {
                                            property: "Capibilities by Resolution: Height"
                                                .to_string(),
                                            error: why.to_string(),
                                        })
                                    }
                                };
                                let value = match capability
                                    .value_by_quark(Quark::from_string("framerate"))
                                {
                                    Ok(v) => match v.transform::<String>() {
                                        Ok(s) => {
                                            format!("{:?}", s)
                                        }
                                        Err(why) => {
                                            return Err(NokhwaError::CouldntQueryDevice {
                                                property: "Framerates".to_string(),
                                                error: format!(
                                                    "Failed to make framerates into string: {}",
                                                    why.to_string()
                                                ),
                                            });
                                        }
                                    },
                                    Err(_) => {
                                        return Err(NokhwaError::CouldntQueryDevice {
                                            property: "Framerates".to_string(),
                                            error: "Failed to get framerates: doesnt exist!"
                                                .to_string(),
                                        })
                                    }
                                };

                                for m in frame_regex.find_iter(&value) {
                                    let fraction_string: Vec<&str> =
                                        m.as_str().split('/').collect();
                                    if fraction_string.len() != 2 {
                                        return Err(NokhwaError::CouldntQueryDevice { property: "Framerates".to_string(), error: format!("Fraction framerate had more than one demoninator: {:?}", fraction_string) });
                                    }

                                    if let Some(v) = fraction_string.get(1) {
                                        if *v != "1" {
                                            continue; // swallow error
                                        }
                                    } else {
                                        return Err(NokhwaError::CouldntQueryDevice { property: "Framerates".to_string(), error: "No framerate denominator? Shouldn't happen, please report!".to_string() });
                                    }

                                    if let Some(numerator) = fraction_string.get(0) {
                                        match numerator.parse::<u32>() {
                                            Ok(fps) => fps_vec.push(fps),
                                            Err(why) => {
                                                return Err(NokhwaError::CouldntQueryDevice {
                                                    property: "Framerates".to_string(),
                                                    error: format!(
                                                        "Failed to parse numerator: {}",
                                                        why.to_string()
                                                    ),
                                                });
                                            }
                                        }
                                    } else {
                                        return Err(NokhwaError::CouldntQueryDevice { property: "Framerates".to_string(), error: "No framerate numerator? Shouldn't happen, please report!".to_string() });
                                    }
                                }
                                resolution_map
                                    .insert(Resolution::new(width as u32, height as u32), fps_vec);
                            }
                        }
                        FrameFormat::YUYV => {
                            if capability.name() == "video/x-raw"
                                && capability.get::<String>("format").unwrap_or_default() == *"YUY2"
                            {
                                let mut fps_vec = vec![];

                                let width = match capability.get::<i32>("width") {
                                    Ok(w) => w,
                                    Err(why) => {
                                        return Err(NokhwaError::CouldntQueryDevice {
                                            property: "Capibilities by Resolution: Width"
                                                .to_string(),
                                            error: why.to_string(),
                                        })
                                    }
                                };
                                let height = match capability.get::<i32>("height") {
                                    Ok(w) => w,
                                    Err(why) => {
                                        return Err(NokhwaError::CouldntQueryDevice {
                                            property: "Capibilities by Resolution: Height"
                                                .to_string(),
                                            error: why.to_string(),
                                        })
                                    }
                                };
                                let value = match capability
                                    .value_by_quark(Quark::from_string("framerate"))
                                {
                                    Ok(v) => match v.transform::<String>() {
                                        Ok(s) => {
                                            format!("{:?}", s)
                                        }
                                        Err(why) => {
                                            return Err(NokhwaError::CouldntQueryDevice {
                                                property: "Framerates".to_string(),
                                                error: format!(
                                                    "Failed to make framerates into string: {}",
                                                    why.to_string()
                                                ),
                                            });
                                        }
                                    },
                                    Err(_) => {
                                        return Err(NokhwaError::CouldntQueryDevice {
                                            property: "Framerates".to_string(),
                                            error: "Failed to get framerates: doesnt exist!"
                                                .to_string(),
                                        })
                                    }
                                };

                                for m in frame_regex.find_iter(&value) {
                                    let fraction_string: Vec<&str> =
                                        m.as_str().split('/').collect();
                                    if fraction_string.len() != 2 {
                                        return Err(NokhwaError::CouldntQueryDevice { property: "Framerates".to_string(), error: format!("Fraction framerate had more than one demoninator: {:?}", fraction_string) });
                                    }

                                    if let Some(v) = fraction_string.get(1) {
                                        if *v != "1" {
                                            continue; // swallow error
                                        }
                                    } else {
                                        return Err(NokhwaError::CouldntQueryDevice { property: "Framerates".to_string(), error: "No framerate denominator? Shouldn't happen, please report!".to_string() });
                                    }

                                    if let Some(numerator) = fraction_string.get(0) {
                                        match numerator.parse::<u32>() {
                                            Ok(fps) => fps_vec.push(fps),
                                            Err(why) => {
                                                return Err(NokhwaError::CouldntQueryDevice {
                                                    property: "Framerates".to_string(),
                                                    error: format!(
                                                        "Failed to parse numerator: {}",
                                                        why.to_string()
                                                    ),
                                                });
                                            }
                                        }
                                    } else {
                                        return Err(NokhwaError::CouldntQueryDevice { property: "Framerates".to_string(), error: "No framerate numerator? Shouldn't happen, please report!".to_string() });
                                    }
                                }
                                resolution_map
                                    .insert(Resolution::new(width as u32, height as u32), fps_vec);
                            }
                        }
                    }
                }
            }
            None => {
                return Err(NokhwaError::CouldntQueryDevice {
                    property: "Device Caps".to_string(),
                    error: "No device caps!".to_string(),
                })
            }
        }

        Ok(resolution_map)
    }

    fn compatible_fourcc(&mut self) -> Result<Vec<FrameFormat>, NokhwaError> {
        let mut format_vec = vec![];
        match self.caps.clone() {
            Some(c) => {
                for capability in c.iter() {
                    if capability.name() == "image/jpeg" {
                        format_vec.push(FrameFormat::MJPEG)
                    } else if capability.name() == "video/x-raw"
                        && capability.get::<String>("format").unwrap_or_default() == *"YUY2"
                    {
                        format_vec.push(FrameFormat::YUYV)
                    }
                }
            }
            None => {
                return Err(NokhwaError::CouldntQueryDevice {
                    property: "Device Caps".to_string(),
                    error: "No device caps!".to_string(),
                })
            }
        }
        format_vec.sort();
        format_vec.dedup();
        Ok(format_vec)
    }

    fn resolution(&self) -> Resolution {
        self.camera_format.resolution()
    }

    fn set_resolution(&mut self, new_res: Resolution) -> Result<(), NokhwaError> {
        let mut new_fmt = self.camera_format;
        new_fmt.set_resolution(new_res);
        self.set_camera_format(new_fmt)
    }

    fn frame_rate(&self) -> u32 {
        self.camera_format.framerate()
    }

    fn set_frame_rate(&mut self, new_fps: u32) -> Result<(), NokhwaError> {
        let mut new_fmt = self.camera_format;
        new_fmt.set_framerate(new_fps);
        self.set_camera_format(new_fmt)
    }

    fn frame_format(&self) -> FrameFormat {
        self.camera_format.format()
    }

    fn set_frame_format(&mut self, _fourcc: FrameFormat) -> Result<(), NokhwaError> {
        Err(NokhwaError::UnsupportedOperation(
            CaptureAPIBackend::GStreamer,
        ))
    }

    fn open_stream(&mut self) -> Result<(), NokhwaError> {
        if let Err(why) = self.pipeline.set_state(State::Playing) {
            return Err(NokhwaError::CouldntOpenStream(format!(
                "Failed to set appsink to playing: {}",
                why.to_string()
            )));
        }
        Ok(())
    }

    // TODO: someone validate this
    fn is_stream_open(&self) -> bool {
        let (res, state_from, state_to) = self.pipeline.state(ClockTime::from_mseconds(16));
        if res.is_ok() {
            if state_to == State::Playing {
                return true;
            }
            false
        } else {
            if state_from == State::Playing {
                return true;
            }
            false
        }
    }

    fn frame(&mut self) -> Result<ImageBuffer<Rgb<u8>, Vec<u8>>, NokhwaError> {
        let image_data = self.frame_raw()?;
        let cam_fmt = self.camera_format;
        let imagebuf =
            match ImageBuffer::from_vec(cam_fmt.width(), cam_fmt.height(), image_data) {
                Some(buf) => {
                    let rgbbuf: ImageBuffer<Rgb<u8>, Vec<u8>> = buf;
                    rgbbuf
                }
                None => return Err(NokhwaError::CouldntCaptureFrame(
                    "Imagebuffer is not large enough! This is probably a bug, please report it!"
                        .to_string(),
                )),
            };
        Ok(imagebuf)
    }

    fn frame_raw(&mut self) -> Result<Vec<u8>, NokhwaError> {
        let bus = match self.pipeline.bus() {
            Some(bus) => bus,
            None => {
                return Err(NokhwaError::CouldntCaptureFrame(
                    "The pipeline has no bus!".to_string(),
                ))
            }
        };

        if let Some(message) = bus.timed_pop(ClockTime::from_seconds(0)) {
            match message.view() {
                MessageView::Eos(..) => {
                    return Err(NokhwaError::CouldntCaptureFrame(
                        "Stream is ended!".to_string(),
                    ))
                }
                MessageView::Error(err) => {
                    return Err(NokhwaError::CouldntCaptureFrame(format!(
                        "Bus error: {}",
                        err.error().to_string()
                    )));
                }
                _ => {}
            }
        }

        match self.receiver.recv() {
            Ok(msg) => Ok(msg.to_vec()),
            Err(why) => {
                return Err(NokhwaError::CouldntCaptureFrame(format!(
                    "Receiver Error: {}",
                    why.to_string()
                )));
            }
        }
    }

    fn stop_stream(&mut self) -> Result<(), NokhwaError> {
        if let Err(why) = self.pipeline.set_state(State::Null) {
            return Err(NokhwaError::CouldntStopStream(format!(
                "Could not change state: {}",
                why.to_string()
            )));
        }
        Ok(())
    }
}

impl Drop for GStreamerCaptureDevice {
    fn drop(&mut self) {
        self.pipeline.set_state(State::Null).unwrap();
    }
}

#[cfg(target_os = "macos")]
fn webcam_pipeline(device: &str, camera_format: CameraFormat) -> String {
    match camera_format.format() {
        FrameFormat::MJPEG => {
            format!("autovideosrc location=/dev/video{} ! image/jpeg,width={},height={},framerate={}/1 ! appsink name=appsink async=false sync=false", device, camera_format.width(), camera_format.height(), camera_format.framerate())
        }
        FrameFormat::YUYV => {
            format!("autovideosrc location=/dev/video{} ! video/x-raw,format=YUY2,width={},height={},framerate={}/1 ! appsink name=appsink async=false sync=false", device, camera_format.width(), camera_format.height(), camera_format.framerate())
        }
    }
}

#[cfg(target_os = "linux")]
fn webcam_pipeline(device: &str, camera_format: CameraFormat) -> String {
    match camera_format.format() {
        FrameFormat::MJPEG => {
            format!("v4l2src device=/dev/video{} ! image/jpeg, width={},height={},framerate={}/1 ! appsink name=appsink async=false sync=false", device, camera_format.width(), camera_format.height(), camera_format.framerate())
        }
        FrameFormat::YUYV => {
            format!("v4l2src device=/dev/video{} ! video/x-raw,format=YUY2,width={},height={},framerate={}/1 ! appsink name=appsink async=false sync=false", device, camera_format.width(), camera_format.height(), camera_format.framerate())
        }
    }
}

#[cfg(target_os = "windows")]
fn webcam_pipeline(device: &str, camera_format: CameraFormat) -> String {
    match camera_format.format() {
        FrameFormat::MJPEG => {
            format!("ksvideosrc device_index={} ! image/jpeg, width={},height={},framerate={}/1 ! appsink name=appsink async=false sync=false", device, camera_format.width(), camera_format.height(), camera_format.framerate())
        }
        FrameFormat::YUYV => {
            format!("ksvideosrc device_index={} ! video/x-raw,format=YUY2,width={},height={},framerate={}/1 ! appsink name=appsink async=false sync=false", device, camera_format.width(), camera_format.height(), camera_format.framerate())
        }
    }
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::let_and_return)]
fn generate_pipeline(fmt: CameraFormat, index: usize) -> Result<PipelineGenRet, NokhwaError> {
    let pipeline =
        match gstreamer::parse_launch(webcam_pipeline(format!("{}", index).as_str(), fmt).as_str())
        {
            Ok(p) => p,
            Err(why) => {
                return Err(NokhwaError::CouldntOpenDevice(format!(
                    "Failed to open pipeline with args {}: {}",
                    webcam_pipeline(format!("{}", index).as_str(), fmt),
                    why.to_string()
                )))
            }
        };

    let sink = match pipeline
        .clone()
        .dynamic_cast::<Bin>()
        .unwrap()
        .by_name("appsink")
    {
        Some(s) => s,
        None => {
            return Err(NokhwaError::CouldntOpenDevice(
                "Failed to get sink element!".to_string(),
            ))
        }
    };

    let appsink = match sink.dynamic_cast::<AppSink>() {
        Ok(aps) => aps,
        Err(_) => {
            return Err(NokhwaError::CouldntOpenDevice(
                "Failed to get sink element as appsink".to_string(),
            ))
        }
    };

    pipeline.set_state(State::Playing).unwrap();

    let (sender, receiver) = flume::unbounded();

    appsink.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |appsink| {
                let sample = appsink.pull_sample().map_err(|_| FlowError::Eos)?;
                let sample_caps = if let Some(c) = sample.caps() {
                    c
                } else {
                    element_error!(
                        appsink,
                        ResourceError::Failed,
                        ("Failed to get caps of sample")
                    );
                    return Err(FlowError::Error);
                };

                let video_info = match VideoInfo::from_caps(sample_caps) {
                    Ok(vi) => vi, // help let me outtttttt
                    Err(why) => {
                        element_error!(
                            appsink,
                            ResourceError::Failed,
                            (format!("Failed to get videoinfo from caps: {}", why.to_string())
                                .as_str())
                        );

                        return Err(FlowError::Error);
                    }
                };

                let buffer = if let Some(buf) = sample.buffer() {
                    buf
                } else {
                    element_error!(
                        appsink,
                        ResourceError::Failed,
                        ("Failed to get buffer from sample")
                    );
                    return Err(FlowError::Error);
                };

                let buffer_map = match buffer.map_readable() {
                    Ok(m) => m,
                    Err(why) => {
                        element_error!(
                            appsink,
                            ResourceError::Failed,
                            (format!("Failed to map buffer to readablemap: {}", why.to_string())
                                .as_str())
                        );

                        return Err(FlowError::Error);
                    }
                };

                let channels = if video_info.has_alpha() { 4 } else { 3 };

                let image_buffer = match video_info.format() {
                    VideoFormat::Yuy2 => {
                        let mut decoded_buffer = match yuyv422_to_rgb888(&buffer_map.as_slice()) {
                            Ok(buf) => buf,
                            Err(why) => {
                                element_error!(
                                    appsink,
                                    ResourceError::Failed,
                                    (format!(
                                        "Failed to make yuy2 into rgb888: {}",
                                        why.to_string()
                                    )
                                    .as_str())
                                );

                                return Err(FlowError::Error);
                            }
                        };

                        decoded_buffer.resize(
                            (video_info.width() * video_info.height() * channels) as usize,
                            0_u8,
                        );

                        let image = if let Some(i) = ImageBuffer::from_vec(
                            video_info.width(),
                            video_info.height(),
                            decoded_buffer,
                        ) {
                            let rgb: ImageBuffer<Rgb<u8>, Vec<u8>> = i;
                            rgb
                        } else {
                            element_error!(
                                appsink,
                                ResourceError::Failed,
                                ("Failed to make rgb buffer into imagebuffer")
                            );

                            return Err(FlowError::Error);
                        };
                        image
                    }
                    VideoFormat::Rgb => {
                        let mut decoded_buffer = buffer_map.as_slice().to_vec();
                        decoded_buffer.resize(
                            (video_info.width() * video_info.height() * channels) as usize,
                            0_u8,
                        );
                        let image = if let Some(i) = ImageBuffer::from_vec(
                            video_info.width(),
                            video_info.height(),
                            decoded_buffer,
                        ) {
                            let rgb: ImageBuffer<Rgb<u8>, Vec<u8>> = i;
                            rgb
                        } else {
                            element_error!(
                                appsink,
                                ResourceError::Failed,
                                ("Failed to make rgb buffer into imagebuffer")
                            );

                            return Err(FlowError::Error);
                        };
                        image
                    }
                    // MJPEG
                    VideoFormat::Encoded => {
                        let mut decoded_buffer = match mjpeg_to_rgb888(&buffer_map.as_slice()) {
                            Ok(buf) => buf,
                            Err(why) => {
                                element_error!(
                                    appsink,
                                    ResourceError::Failed,
                                    (format!(
                                        "Failed to make yuy2 into rgb888: {}",
                                        why.to_string()
                                    )
                                    .as_str())
                                );

                                return Err(FlowError::Error);
                            }
                        };

                        decoded_buffer.resize(
                            (video_info.width() * video_info.height() * channels) as usize,
                            0_u8,
                        );

                        let image = if let Some(i) = ImageBuffer::from_vec(
                            video_info.width(),
                            video_info.height(),
                            decoded_buffer,
                        ) {
                            let rgb: ImageBuffer<Rgb<u8>, Vec<u8>> = i;
                            rgb
                        } else {
                            element_error!(
                                appsink,
                                ResourceError::Failed,
                                ("Failed to make rgb buffer into imagebuffer")
                            );

                            return Err(FlowError::Error);
                        };
                        image
                    }
                    _ => {
                        element_error!(
                            appsink,
                            ResourceError::Failed,
                            ("Unsupported video format")
                        );
                        return Err(FlowError::Error);
                    }
                };

                if sender.send(image_buffer).is_err() {
                    return Err(FlowError::Error);
                }

                Ok(FlowSuccess::Ok)
            })
            .build(),
    );
    Ok((pipeline, appsink, receiver))
}