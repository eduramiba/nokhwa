/*
 * Copyright 2022 l1npengtul <l1npengtul@protonmail.com> / The Nokhwa Contributors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::fmt::{Display, Formatter};

/// Describes a frame format (i.e. how the bytes themselves are encoded). Often called `FourCC`.
#[derive(Copy, Clone, Debug, Hash, Ord, PartialOrd, Eq, PartialEq)]
#[cfg_attr(feature = "serialize", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum FrameFormat {
    // Compressed Formats
    H265,
    H264,
    Avc1,
    H263,
    Av1,
    Mpeg1,
    Mpeg2,
    Mpeg4,
    MJpeg,
    XVid,
    VP8,
    VP9,

    // YCbCr Formats

    // 8 bit per pixel, 4:4:4
    Ayuv444,

    // -> 4:2:2
    Yuyv422, // AKA YUY2
    Uyvy422, // UYUV
    Yvyu422,
    Yv12,

    // 4:2:0
    Nv12,
    Nv21,
    I420,

    // 16:1:1
    Yvu9,

    // Grayscale Formats
    Luma8,
    Luma16,

    // Depth
    Depth16,

    // RGB Formats
    Rgb332,
    Rgb555,
    Rgb565,

    Rgb888,

    RgbA8888,
    ARgb8888,

    // Bayer Formats
    Bayer8,
    Bayer16,

    // Custom
    Custom([u8; 8]),
}

// FIXME: Fix these frame format lists! Maybe move to using a macro..?
impl FrameFormat {
    pub const ALL: &'static [FrameFormat] = &[
        FrameFormat::H263,
        FrameFormat::H264,
        FrameFormat::H265,
        FrameFormat::Av1,
        FrameFormat::Avc1,
        FrameFormat::Mpeg1,
        FrameFormat::Mpeg2,
        FrameFormat::Mpeg4,
        FrameFormat::MJpeg,
        FrameFormat::XVid,
        FrameFormat::VP8,
        FrameFormat::VP9,
        FrameFormat::Yuyv422,
        FrameFormat::Uyvy422,
        FrameFormat::Nv12,
        FrameFormat::Nv21,
        FrameFormat::Yv12,
        FrameFormat::Luma8,
        FrameFormat::Luma16,
        FrameFormat::Rgb332,
        FrameFormat::RgbA8888,
    ];

    pub const COMPRESSED: &'static [FrameFormat] = &[
        FrameFormat::H263,
        FrameFormat::H264,
        FrameFormat::H265,
        FrameFormat::Av1,
        FrameFormat::Avc1,
        FrameFormat::Mpeg1,
        FrameFormat::Mpeg2,
        FrameFormat::Mpeg4,
        FrameFormat::MJpeg,
        FrameFormat::XVid,
        FrameFormat::VP8,
        FrameFormat::VP9,
    ];

    pub const CHROMA: &'static [FrameFormat] = &[
        FrameFormat::Yuyv422,
        FrameFormat::Uyvy422,
        FrameFormat::Nv12,
        FrameFormat::Nv21,
        FrameFormat::Yv12,
    ];

    pub const LUMA: &'static [FrameFormat] = &[FrameFormat::Luma8, FrameFormat::Luma16];

    pub const RGB: &'static [FrameFormat] = &[FrameFormat::Rgb332, FrameFormat::RgbA8888];

    pub const COLOR_FORMATS: &'static [FrameFormat] = &[
        FrameFormat::H265,
        FrameFormat::H264,
        FrameFormat::H263,
        FrameFormat::Av1,
        FrameFormat::Avc1,
        FrameFormat::Mpeg1,
        FrameFormat::Mpeg2,
        FrameFormat::Mpeg4,
        FrameFormat::MJpeg,
        FrameFormat::XVid,
        FrameFormat::VP8,
        FrameFormat::VP9,
        FrameFormat::Yuyv422,
        FrameFormat::Uyvy422,
        FrameFormat::Nv12,
        FrameFormat::Nv21,
        FrameFormat::Yv12,
        FrameFormat::Rgb332,
        FrameFormat::RgbA8888,
    ];

    pub const GRAYSCALE: &'static [FrameFormat] = &[FrameFormat::Luma8, FrameFormat::Luma16];
}

impl Display for FrameFormat {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[macro_export]
macro_rules! define_back_and_fourth_frame_format {
    ($fourcc_type:ty, { $( $frame_format:expr => $value:literal, )* }, $func_u8_8_to_fcc:expr, $func_fcc_to_u8_8:expr, $value_to_fcc_type:expr) => {
        pub struct FrameFormatIntermediate(pub $fourcc_type);

        impl FrameFormatIntermediate {
            pub fn from_frame_format(frame_format: FrameFormat) -> Option<Self> {
                match frame_format {
                    $(
                        $frame_format => Some(Self($value_to_fcc_type($value))),
                    )*
                    FrameFormat::Custom(cv) => Some($func_u8_8_to_fcc(cv))
                    _ => None,
                }
            }

            pub fn into_frame_format(fourcc: $fourcc_type) -> FrameFormat {
                match fourcc.0 {
                    $(
                         $value => $frame_format,
                    )*
                    cv => FrameFormat::Custom($func_fcc_to_u8_8(cv)),
                }
            }
        }
    };
}
