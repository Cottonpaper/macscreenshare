//! Video decoding, behind a stable trait so the backend can change.
//!
//! The default build ships [`NullDecoder`], which is enough for protocol and
//! transport tests. Enable the `ffmpeg` feature for real decoding; see
//! `client/README.md` for the required system libraries.

use anyhow::Result;

pub mod synthetic;

/// Pixel layout of a decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Planar YUV 4:2:0. Planes: Y, U, V.
    Yuv420p,
    /// Semi-planar YUV 4:2:0. Planes: Y, interleaved UV.
    Nv12,
}

/// One image plane: contiguous bytes plus row stride.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
}

/// A decoded, displayable frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub planes: Vec<Plane>,
    pub pts_ms: u32,
}

/// Receives decoded frames, typically implemented by the renderer.
pub trait FrameSink: Send + Sync + 'static {
    fn submit(&self, frame: DecodedFrame);
}

/// Decodes access units produced by the host.
pub trait VideoDecoder: Send {
    /// Feed one access unit (Annex-B for H.264/HEVC).
    fn decode(&mut self, packet: &[u8], keyframe: bool, pts_ms: u32) -> Result<Vec<DecodedFrame>>;

    /// Drain any frames buffered inside the decoder.
    fn flush(&mut self) -> Result<Vec<DecodedFrame>>;
}

/// Counts packets and returns no frames. Used for tests, dry runs and CI.
#[derive(Debug, Default)]
pub struct NullDecoder {
    packets: u64,
    keyframes: u64,
    bytes: u64,
}

impl NullDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn packets(&self) -> u64 {
        self.packets
    }

    pub fn keyframes(&self) -> u64 {
        self.keyframes
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl VideoDecoder for NullDecoder {
    fn decode(&mut self, packet: &[u8], keyframe: bool, _pts_ms: u32) -> Result<Vec<DecodedFrame>> {
        self.packets += 1;
        self.bytes += packet.len() as u64;
        if keyframe {
            self.keyframes += 1;
        }
        Ok(Vec::new())
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        Ok(Vec::new())
    }
}

/// Build the decoder selected by build features.
pub fn default_decoder() -> Result<Box<dyn VideoDecoder>> {
    #[cfg(feature = "ffmpeg")]
    {
        return Ok(Box::new(ffmpeg_backend::FfmpegDecoder::new_h264()?));
    }
    #[cfg(not(feature = "ffmpeg"))]
    {
        tracing::warn!(
            "built without the `ffmpeg` feature: video packets will be counted but not decoded"
        );
        return Ok(Box::new(NullDecoder::new()));
    }
}

/// Human-readable name of the active decoder backend.
pub fn backend_name() -> &'static str {
    #[cfg(feature = "ffmpeg")]
    {
        return "ffmpeg";
    }
    #[cfg(not(feature = "ffmpeg"))]
    {
        return "null (packets counted only)";
    }
}

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg_backend {
    use anyhow::{anyhow, Context, Result};
    use ffmpeg_next as ffmpeg;
    use ffmpeg::format::Pixel;
    use ffmpeg::software::scaling::{context::Context as ScaleContext, Flags as ScaleFlags};
    use ffmpeg::util::frame::Video as VideoFrame;

    use super::{DecodedFrame, PixelFormat, Plane, VideoDecoder};

    pub struct FfmpegDecoder {
        dec: ffmpeg::decoder::Video,
        scaler: Option<ScaleContext>,
        h264: bool,
    }

    impl FfmpegDecoder {
        pub fn new_h264() -> Result<Self> {
            Self::new(ffmpeg::codec::Id::H264)
        }

        pub fn new_hevc() -> Result<Self> {
            Self::new(ffmpeg::codec::Id::HEVC)
        }

        fn new(id: ffmpeg::codec::Id) -> Result<Self> {
            ffmpeg::init().context("ffmpeg::init")?;
            let codec = ffmpeg::decoder::find(id)
                .ok_or_else(|| anyhow!("no ffmpeg decoder for {id:?}"))?;
            let ctx = ffmpeg::codec::context::Context::new_with_codec(codec);
            let dec = ctx.decoder().video().context("open video decoder")?;
            Ok(Self {
                dec,
                scaler: None,
                h264: id == ffmpeg::codec::Id::H264,
            })
        }

        fn convert(&mut self, frame: &VideoFrame, pts_ms: u32) -> Result<DecodedFrame> {
            let (w, h) = (frame.width(), frame.height());
            if frame.format() == Pixel::YUV420P {
                return Ok(copy_yuv420p(frame, pts_ms));
            }
            if frame.format() == Pixel::NV12 {
                return Ok(nv12_to_yuv420p(frame, pts_ms));
            }

            // Rebuild the scaler if the geometry or pixel format changed.
            let needs_scaler = match &self.scaler {
                Some(s) => s.input().width != w || s.input().height != h,
                None => true,
            };
            if needs_scaler {
                self.scaler = Some(
                    ScaleContext::get(
                        frame.format(),
                        w,
                        h,
                        Pixel::YUV420P,
                        w,
                        h,
                        ScaleFlags::BILINEAR,
                    )
                    .context("create scaler")?,
                );
            }
            let mut out = VideoFrame::empty();
            self.scaler
                .as_mut()
                .unwrap()
                .run(frame, &mut out)
                .context("scale frame")?;
            Ok(copy_yuv420p(&out, pts_ms))
        }
    }

    /// Extract the three planes of a planar YUV 4:2:0 frame.
    fn copy_yuv420p(frame: &VideoFrame, pts_ms: u32) -> DecodedFrame {
        DecodedFrame {
            width: frame.width(),
            height: frame.height(),
            format: PixelFormat::Yuv420p,
            planes: (0..3).map(|i| copy_plane(frame, i)).collect(),
            pts_ms,
        }
    }

    /// Split an NV12 frame into separate Y, U and V planes.
    fn nv12_to_yuv420p(frame: &VideoFrame, pts_ms: u32) -> DecodedFrame {
        let width = frame.width();
        let height = frame.height();
        let chroma_rows = (height as usize).div_ceil(2);
        let chroma_cols = (width as usize).div_ceil(2);

        let y = copy_plane(frame, 0);

        let uv = frame.data(1);
        let uv_stride = frame.stride(1);
        let mut u = Vec::with_capacity(chroma_cols * chroma_rows);
        let mut v = Vec::with_capacity(chroma_cols * chroma_rows);
        for row in 0..chroma_rows {
            let base = row * uv_stride;
            for col in 0..chroma_cols {
                let idx = base + col * 2;
                if idx + 1 < uv.len() {
                    u.push(uv[idx]);
                    v.push(uv[idx + 1]);
                } else {
                    u.push(128);
                    v.push(128);
                }
            }
        }

        DecodedFrame {
            width,
            height,
            format: PixelFormat::Yuv420p,
            planes: vec![
                y,
                Plane { data: u, stride: chroma_cols },
                Plane { data: v, stride: chroma_cols },
            ],
            pts_ms,
        }
    }

    fn copy_plane(frame: &VideoFrame, index: usize) -> Plane {
        let stride = frame.stride(index);
        let rows = if index == 0 {
            frame.height() as usize
        } else {
            (frame.height() as usize).div_ceil(2)
        };
        let data = frame.data(index);
        // Copy tightly per row so downstream code need not honour stride.
        let mut out = Vec::with_capacity(stride.min(data.len().max(1)) * rows);
        for row in 0..rows {
            let start = row * stride;
            let end = (start + stride).min(data.len());
            if start >= end {
                break;
            }
            out.extend_from_slice(&data[start..end]);
        }
        Plane { data: out, stride }
    }

    impl VideoDecoder for FfmpegDecoder {
        fn decode(&mut self, packet: &[u8], _keyframe: bool, pts_ms: u32) -> Result<Vec<DecodedFrame>> {
            let pkt = ffmpeg::Packet::copy(packet);
            self.dec.send_packet(&pkt).context("send packet")?;
            let mut frames = Vec::new();
            let mut frame = VideoFrame::empty();
            while self.dec.receive_frame(&mut frame).is_ok() {
                frames.push(self.convert(&frame, pts_ms)?);
            }
            Ok(frames)
        }

        fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
            self.dec.send_eof().ok();
            let mut frames = Vec::new();
            let mut frame = VideoFrame::empty();
            while self.dec.receive_frame(&mut frame).is_ok() {
                frames.push(self.convert(&frame, 0)?);
            }
            Ok(frames)
        }
    }

    /// Reserved for HEVC support once the host exposes a codec switch.
    pub fn _hevc_available() -> bool {
        ffmpeg::decoder::find(ffmpeg::codec::Id::HEVC).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_decoder_counts() {
        let mut d = NullDecoder::new();
        d.decode(&[0u8; 10], true, 0).unwrap();
        d.decode(&[0u8; 5], false, 1).unwrap();
        assert_eq!(d.packets(), 2);
        assert_eq!(d.keyframes(), 1);
        assert_eq!(d.bytes(), 15);
    }

    #[test]
    fn default_decoder_builds() {
        // Without the ffmpeg feature this yields a NullDecoder.
        assert!(default_decoder().is_ok());
    }
}
