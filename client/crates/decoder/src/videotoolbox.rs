//! Native H.264/HEVC decode via macOS VideoToolbox (`VTDecompressionSession`).
//!
//! Compiled only for `cfg(all(feature = "videotoolbox", target_os = "macos"))`;
//! see `docs/videotoolbox-decode.md`. The host prefixes chunk 0 of every
//! keyframe with the parameter sets, so the decoder configures itself from the
//! stream with no out-of-band setup.
//!
//! VideoToolbox is asynchronous, but with no decode flags the output handler is
//! invoked before `decode_frame_with_output_handler` returns, so a frame-at-a-
//! time blocking design is both simple and correct here.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use block2::RcBlock;
use objc2_core_foundation::{
    kCFAllocatorNull, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets,
};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetPlaneCount,
    CVPixelBufferGetWidth, CVPixelBufferIsPlanar, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{
    kVTDecompressionPropertyKey_RealTime,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder, VTDecodeFrameFlags,
    VTDecodeInfoFlags, VTDecompressionSession, VTSessionSetProperty,
};

use macss_protocol::Codec;

use crate::annexb::{self, split_nals, split_parameter_sets, to_avcc};
use crate::planes;
use crate::{DecodedFrame, VideoDecoder};

/// `noErr` for Core* / VideoToolbox status codes.
const NO_ERR: i32 = 0;
/// NAL length prefix width we build with `to_avcc`, and hand to VideoToolbox.
const NAL_LENGTH_SIZE: i32 = 4;

/// Hardware (or software) VideoToolbox decoder for H.264 and HEVC.
pub struct VideoToolboxDecoder {
    codec: Option<Codec>,
    /// Parameter-set bytes that produced `session`/`format`.
    format_signature: Vec<Vec<u8>>,
    format: Option<CFRetained<CMFormatDescription>>,
    session: Option<CFRetained<VTDecompressionSession>>,
}

impl VideoToolboxDecoder {
    pub fn new() -> Result<Self> {
        Ok(Self {
            codec: None,
            format_signature: Vec::new(),
            format: None,
            session: None,
        })
    }

    /// Infer the codec from a keyframe's parameter sets.
    fn detect_codec(nals: &[&[u8]]) -> Option<Codec> {
        if nals
            .iter()
            .any(|nal| annexb::h264_nal_type(nal) == Some(7))
        {
            Some(Codec::H264)
        } else if nals
            .iter()
            .any(|nal| matches!(annexb::hevc_nal_type(nal), Some(32 | 33)))
        {
            Some(Codec::Hevc)
        } else {
            None
        }
    }

    /// Build (or rebuild) the format description and session for new parameter
    /// sets. Cheap to call on every keyframe; only reconfigures on real change.
    fn ensure_session(&mut self, codec: Codec, sets: &[&[u8]]) -> Result<()> {
        let signature: Vec<Vec<u8>> = sets.iter().map(|set| set.to_vec()).collect();
        if self.session.is_some() && signature == self.format_signature {
            return Ok(());
        }

        let format = create_format_description(codec, sets)?;
        let session = create_session(&format)?;

        self.codec = Some(codec);
        self.format_signature = signature;
        self.format = Some(format);
        self.session = Some(session);

        tracing::info!(
            "videotoolbox: configured {} decoder from {} parameter set(s)",
            codec_name(codec),
            sets.len()
        );
        Ok(())
    }

    /// Hand one AVCC access unit to the session and collect any output frames.
    fn decode_slices(&self, slices: &[&[u8]], pts_ms: u32) -> Result<Vec<DecodedFrame>> {
        let (session, format) = match (&self.session, &self.format) {
            (Some(session), Some(format)) => (session, format),
            // No keyframe seen yet: nothing to decode into.
            _ => return Ok(Vec::new()),
        };
        if slices.is_empty() {
            return Ok(Vec::new());
        }

        let avcc = to_avcc(slices);
        if avcc.is_empty() {
            return Ok(Vec::new());
        }

        // A block buffer that borrows `avcc`; `kCFAllocatorNull` stops
        // CoreMedia from freeing Rust-owned memory. `avcc` outlives the decode
        // call below, so the borrow is valid for as long as it is used.
        let mut block_ptr: *mut CMBlockBuffer = ptr::null_mut();
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                avcc.as_ptr() as *mut c_void,
                avcc.len(),
                kCFAllocatorNull,
                ptr::null(),
                0,
                avcc.len(),
                0,
                NonNull::new(&mut block_ptr).unwrap(),
            )
        };
        if status != NO_ERR || block_ptr.is_null() {
            return Err(anyhow!("CMBlockBufferCreateWithMemoryBlock failed ({status})"));
        }
        let block = unsafe { CFRetained::from_raw(NonNull::new(block_ptr).unwrap()) };

        let mut sample_ptr: *mut CMSampleBuffer = ptr::null_mut();
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&**format),
                1,
                0,
                ptr::null(),
                0,
                ptr::null(),
                NonNull::new(&mut sample_ptr).unwrap(),
            )
        };
        if status != NO_ERR || sample_ptr.is_null() {
            return Err(anyhow!("CMSampleBufferCreateReady failed ({status})"));
        }
        let sample = unsafe { CFRetained::from_raw(NonNull::new(sample_ptr).unwrap()) };

        // Collect from the (potentially async) handler into shared storage.
        let collected: Arc<Mutex<Vec<DecodedFrame>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&collected);
        let handler = RcBlock::new(
            move |status: i32,
                  _flags: VTDecodeInfoFlags,
                  image: *mut CVImageBuffer,
                  _pts: CMTime,
                  _duration: CMTime| {
                if status != NO_ERR || image.is_null() {
                    return;
                }
                if let Some(frame) = frame_from_image(image, pts_ms) {
                    if let Ok(mut frames) = sink.lock() {
                        frames.push(frame);
                    }
                }
            },
        );

        let mut info_flags = VTDecodeInfoFlags(0);
        let status = unsafe {
            session.decode_frame_with_output_handler(
                &sample,
                VTDecodeFrameFlags(0),
                &mut info_flags,
                RcBlock::as_ptr(&handler),
            )
        };

        // Keep everything referenced by the decode alive until it has returned.
        drop(handler);
        drop(sample);
        drop(block);

        if status != NO_ERR {
            tracing::warn!("videotoolbox: decode failed (status {status})");
            return Ok(Vec::new());
        }

        let frames = match collected.lock() {
            Ok(frames) => frames.clone(),
            Err(_) => Vec::new(),
        };
        Ok(frames)
    }
}

// SAFETY: `VTDecompressionSession` is not `Send`/`Sync` (it is a CoreFoundation
// object), but this decoder owns its session exclusively and only ever touches
// it from the single task that drives the session. `VideoDecoder: Send` only
// requires that the value can move between threads, never that it is shared.
unsafe impl Send for VideoToolboxDecoder {}

impl VideoDecoder for VideoToolboxDecoder {
    fn decode(&mut self, packet: &[u8], keyframe: bool, pts_ms: u32) -> Result<Vec<DecodedFrame>> {
        let nals = split_nals(packet);
        if nals.is_empty() {
            return Ok(Vec::new());
        }

        if keyframe {
            let codec = match Self::detect_codec(&nals).or(self.codec) {
                Some(codec) => codec,
                None => return Ok(Vec::new()),
            };
            let (sets, slices) = split_parameter_sets(codec, &nals);
            if !sets.is_empty() {
                self.ensure_session(codec, &sets)?;
            }
            return self.decode_slices(&slices, pts_ms);
        }

        // A slice-only packet still needs the codec to separate any stray
        // parameter sets; without a prior keyframe there is nothing to do.
        let codec = match self.codec {
            Some(codec) => codec,
            None => return Ok(Vec::new()),
        };
        let (_sets, slices) = split_parameter_sets(codec, &nals);
        self.decode_slices(&slices, pts_ms)
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        if let Some(session) = &self.session {
            unsafe {
                session.wait_for_asynchronous_frames();
            }
        }
        // The blocking decode in `decode` already drained every frame, so
        // nothing is buffered here.
        Ok(Vec::new())
    }
}

fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
    }
}

/// Build a `CMFormatDescription` from raw (start-code-free) parameter sets.
fn create_format_description(
    codec: Codec,
    sets: &[&[u8]],
) -> Result<CFRetained<CMFormatDescription>> {
    let mut pointers: Vec<NonNull<u8>> = sets
        .iter()
        .map(|set| {
            NonNull::new(set.as_ptr() as *mut u8).ok_or_else(|| anyhow!("empty parameter set"))
        })
        .collect::<Result<_>>()?;
    let mut sizes: Vec<usize> = sets.iter().map(|set| set.len()).collect();

    let mut out: *const CMFormatDescription = ptr::null();
    let out_ptr = NonNull::new(&mut out).unwrap();
    let pointers_ptr = NonNull::new(pointers.as_mut_ptr()).unwrap();
    let sizes_ptr = NonNull::new(sizes.as_mut_ptr()).unwrap();

    let status = unsafe {
        match codec {
            Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                sets.len(),
                pointers_ptr,
                sizes_ptr,
                NAL_LENGTH_SIZE,
                out_ptr,
            ),
            Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                sets.len(),
                pointers_ptr,
                sizes_ptr,
                NAL_LENGTH_SIZE,
                None,
                out_ptr,
            ),
        }
    };

    if status != NO_ERR || out.is_null() {
        return Err(anyhow!(
            "could not build {} format description ({status})",
            codec_name(codec)
        ));
    }
    Ok(unsafe { CFRetained::from_raw(NonNull::new(out as *mut CMFormatDescription).unwrap()) })
}

/// Create a decompression session requesting hardware decode and NV12 output.
fn create_session(format: &CMFormatDescription) -> Result<CFRetained<VTDecompressionSession>> {
    let hardware_key: &CFString =
        unsafe { kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder };
    let hardware_value: &CFType = CFBoolean::new(true);
    let specification = CFDictionary::from_slices(&[hardware_key], &[hardware_value]);

    let format_key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
    let format_number =
        CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
    let format_value: &CFType = &format_number;
    let attributes = CFDictionary::from_slices(&[format_key], &[format_value]);

    let specification_ref: &CFDictionary = AsRef::as_ref(&*specification);
    let attributes_ref: &CFDictionary = AsRef::as_ref(&*attributes);

    let mut out: *mut VTDecompressionSession = ptr::null_mut();
    let status = unsafe {
        VTDecompressionSession::create(
            None,
            format,
            Some(specification_ref),
            Some(attributes_ref),
            ptr::null(),
            NonNull::new(&mut out).unwrap(),
        )
    };
    if status != NO_ERR || out.is_null() {
        return Err(anyhow!("VTDecompressionSessionCreate failed ({status})"));
    }
    let session = unsafe { CFRetained::from_raw(NonNull::new(out).unwrap()) };

    let status = unsafe {
        VTSessionSetProperty(
            &session,
            kVTDecompressionPropertyKey_RealTime,
            Some(CFBoolean::new(true)),
        )
    };
    if status != NO_ERR {
        tracing::warn!("videotoolbox: could not enable realtime mode (status {status})");
    }

    Ok(session)
}

/// Lock the decoded `CVPixelBuffer` (NV12) and copy it into a planar frame.
fn frame_from_image(image: *mut CVImageBuffer, pts_ms: u32) -> Option<DecodedFrame> {
    // `CVImageBuffer` is a type alias for `CVPixelBuffer`.
    let pixel: &CVPixelBuffer = unsafe { &*image };

    let lock_flags = CVPixelBufferLockFlags(0);
    if unsafe { CVPixelBufferLockBaseAddress(pixel, lock_flags) } != 0 {
        return None;
    }
    let frame = read_nv12(pixel, pts_ms);
    unsafe { CVPixelBufferUnlockBaseAddress(pixel, lock_flags) };
    frame
}

/// Copy the two NV12 planes out of a locked pixel buffer.
fn read_nv12(pixel: &CVPixelBuffer, pts_ms: u32) -> Option<DecodedFrame> {
    let width = CVPixelBufferGetWidth(pixel) as u32;
    let height = CVPixelBufferGetHeight(pixel) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    if !CVPixelBufferIsPlanar(pixel) || CVPixelBufferGetPlaneCount(pixel) < 2 {
        tracing::warn!("videotoolbox: unexpected non-planar output buffer");
        return None;
    }

    let y_base = CVPixelBufferGetBaseAddressOfPlane(pixel, 0) as *const u8;
    let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel, 0);
    let uv_base = CVPixelBufferGetBaseAddressOfPlane(pixel, 1) as *const u8;
    let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel, 1);
    if y_base.is_null() || uv_base.is_null() || y_stride == 0 || uv_stride == 0 {
        return None;
    }

    let rows = height as usize;
    let chroma_rows = rows.div_ceil(2);
    // SAFETY: the buffer is locked and the plane base addresses are valid for
    // at least `stride * rows` bytes.
    let y = unsafe { std::slice::from_raw_parts(y_base, y_stride * rows) };
    let uv = unsafe { std::slice::from_raw_parts(uv_base, uv_stride * chroma_rows) };

    Some(planes::from_nv12(
        width, height, y, y_stride, uv, uv_stride, pts_ms,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annex_b(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn detects_h264_from_sps() {
        // H.264 SPS/PPS: low 5 bits of the first byte are 7 and 8.
        let stream = annex_b(&[&[0x67, 0x42, 0x00, 0x1F], &[0x68, 0xCE], &[0x65, 0x88]]);
        let nals = split_nals(&stream);
        assert_eq!(VideoToolboxDecoder::detect_codec(&nals), Some(Codec::H264));
    }

    #[test]
    fn detects_hevc_from_vps_and_sps() {
        let stream = annex_b(&[&[0x40, 0x01], &[0x42, 0x01], &[0x26, 0x01]]);
        let nals = split_nals(&stream);
        assert_eq!(VideoToolboxDecoder::detect_codec(&nals), Some(Codec::Hevc));
    }

    /// Detection is only run on keyframes (which the host prefixes with real
    /// parameter sets); a bare H.264 slice byte such as `0x41` can share its
    /// bit pattern with an HEVC VPS, so slice-only input is not used to pick a
    /// codec.
    #[test]
    fn detects_nothing_without_parameter_sets() {
        let stream = annex_b(&[&[0x65, 0x88], &[0x61, 0x9A]]);
        let nals = split_nals(&stream);
        assert_eq!(VideoToolboxDecoder::detect_codec(&nals), None);
    }

    #[test]
    fn constructs_before_any_stream() {
        assert!(VideoToolboxDecoder::new().is_ok());
    }
}
