//! Media Foundation (Windows) H.264/HEVC decoder.
//!
//! Drives the OS decoder MFTs directly — `CLSID_CMSH264DecoderMFT` and
//! `CLSID_CMSH265DecoderMFT` (the latter needs the "HEVC Video Extensions").
//! Output is NV12, converted with [`crate::planes::from_nv12`]. No FFmpeg.
//!
//! The host sends Annex-B access units; the parameter sets on each keyframe are
//! pulled out and handed to the MFT as `MF_MT_MPEG_SEQUENCE_HEADER`.

use std::cell::Cell;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use macss_protocol::Codec;
use windows::core::{GUID, Interface};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use crate::annexb;
use crate::planes;
use crate::{DecodedFrame, VideoDecoder};

/// ITU-T H.264 decoder MFT (`CLSID_CMSH264DecoderMFT`).
const CLSID_H264_DECODER_MFT: GUID = GUID::from_u128(0x62CE_7E72_4C71_4D20_B15D_4528_31A8_7D9D);
/// H.265/HEVC decoder MFT (`CLSID_CMSH265DecoderMFT`).
const CLSID_HEVC_DECODER_MFT: GUID = GUID::from_u128(0x420A_51A3_D605_430C_B4FC_4527_4FA6_C562);

/// 100-ns ticks per millisecond (Media Foundation sample times are in 100 ns).
const TICKS_PER_MS: i64 = 10_000;
/// Fallback sample duration (100 ns) — 1/60 s, when a frame has no predecessor.
const DEFAULT_DURATION: i64 = 10_000_000 / 60;

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// Initialise COM (MTA) on this thread and Media Foundation once per process.
fn ensure_platform() {
    COM_READY.with(|ready| {
        if !ready.get() {
            // MTA: the decoder MFT is free-threaded and the session task may
            // migrate between runtime threads.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            ready.set(true);
        }
    });
    static MF_READY: OnceLock<()> = OnceLock::new();
    MF_READY.get_or_init(|| unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
    });
}

/// Native Windows decoder backed by a Media Foundation decoder MFT.
pub struct MediaFoundationDecoder {
    transform: IMFTransform,
    codec: Codec,
    /// Output type negotiated from the first keyframe.
    configured: bool,
    width: u32,
    height: u32,
    output_provides_samples: bool,
    last_pts_ms: Option<u32>,
}

// SAFETY: `ensure_platform()` initialises COM (MTA) on whichever thread calls
// `decode`, and the decoder MFT is a free-threaded object, so the wrapper can
// move between runtime threads. The `windows` interface pointers are not `Send`
// on their own.
unsafe impl Send for MediaFoundationDecoder {}

impl MediaFoundationDecoder {
    pub fn new(codec: Codec) -> Result<Self> {
        ensure_platform();
        let clsid = match codec {
            Codec::H264 => CLSID_H264_DECODER_MFT,
            Codec::Hevc => CLSID_HEVC_DECODER_MFT,
        };
        let subtype = match codec {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
        };

        unsafe {
            let transform: IMFTransform = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)
                .context("create Media Foundation decoder MFT")?;

            let input = MFCreateMediaType().context("MFCreateMediaType")?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            transform
                .SetInputType(0, Some(&input), 0)
                .context("SetInputType")?;

            let info = transform
                .GetOutputStreamInfo(0)
                .context("GetOutputStreamInfo")?;
            let output_provides_samples =
                info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

            Ok(Self {
                transform,
                codec,
                configured: false,
                width: 0,
                height: 0,
                output_provides_samples,
                last_pts_ms: None,
            })
        }
    }

    fn subtype(&self) -> GUID {
        match self.codec {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
        }
    }

    /// Negotiate the output type once the first keyframe has supplied SPS/PPS.
    fn configure(&mut self, keyframe: &[u8]) -> Result<()> {
        let nals = annexb::split_nals(keyframe);
        let (sets, _) = annexb::split_parameter_sets(self.codec, &nals);
        if sets.is_empty() {
            return Err(anyhow!("keyframe carried no parameter sets"));
        }
        // MF wants the parameter sets as one Annex-B blob.
        let mut sequence_header = Vec::new();
        for nal in &sets {
            sequence_header.extend_from_slice(&[0, 0, 0, 1]);
            sequence_header.extend_from_slice(nal);
        }

        unsafe {
            let subtype = self.subtype();
            let input = MFCreateMediaType().context("MFCreateMediaType")?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            input.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &sequence_header)?;
            self.transform
                .SetInputType(0, Some(&input), 0)
                .context("SetInputType (config)")?;
        }

        self.select_output_type()?;

        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .context("BeginStreaming")?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .context("StartOfStream")?;
        }
        self.configured = true;
        tracing::info!(
            "mediafoundation: configured {:?} decoder, {}x{} NV12",
            self.codec,
            self.width,
            self.height
        );
        Ok(())
    }

    /// Pick an NV12 output type and read its frame size. Re-run on stream change.
    fn select_output_type(&mut self) -> Result<()> {
        unsafe {
            let mut index = 0u32;
            loop {
                let candidate = self
                    .transform
                    .GetOutputAvailableType(0, index)
                    .with_context(|| format!("GetOutputAvailableType #{index}"))?;
                let is_nv12 = candidate
                    .GetGUID(&MF_MT_SUBTYPE)
                    .map(|g| g == MFVideoFormat_NV12)
                    .unwrap_or(false);
                if is_nv12 {
                    self.transform
                        .SetOutputType(0, Some(&candidate), 0)
                        .context("SetOutputType")?;
                    let size = candidate.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                    self.width = (size >> 32) as u32;
                    self.height = (size & 0xFFFF_FFFF) as u32;
                    if self.width == 0 || self.height == 0 {
                        return Err(anyhow!("decoder output type has no frame size"));
                    }
                    return Ok(());
                }
                index += 1;
            }
        }
    }

    fn push_sample(&mut self, au: &[u8], pts_ms: u32) -> Result<()> {
        let duration = self
            .last_pts_ms
            .map(|previous| (pts_ms.saturating_sub(previous) as i64) * TICKS_PER_MS)
            .filter(|d| *d > 0)
            .unwrap_or(DEFAULT_DURATION);
        self.last_pts_ms = Some(pts_ms);

        unsafe {
            let buffer =
                MFCreateMemoryBuffer(au.len() as u32).context("MFCreateMemoryBuffer")?;
            {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None).context("buffer lock")?;
                std::ptr::copy_nonoverlapping(au.as_ptr(), ptr, au.len());
                buffer.Unlock().ok();
                buffer.SetCurrentLength(au.len() as u32)?;
            }

            let sample = MFCreateSample().context("MFCreateSample")?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_ms as i64 * TICKS_PER_MS)?;
            sample.SetSampleDuration(duration)?;
            self.transform
                .ProcessInput(0, Some(&sample), 0)
                .context("ProcessInput")?;
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<DecodedFrame>> {
        let mut frames = Vec::new();
        loop {
            let mut output = MFT_OUTPUT_DATA_BUFFER::default();
            if !self.output_provides_samples {
                unsafe {
                    let bytes = (self.width as usize * self.height as usize * 3 / 2).max(1);
                    let buffer = MFCreateMemoryBuffer(bytes as u32).context("out buffer")?;
                    let sample = MFCreateSample().context("out sample")?;
                    sample.AddBuffer(&buffer)?;
                    output.pSample = std::mem::ManuallyDrop::new(Some(sample));
                }
            }

            let mut status = 0u32;
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status)
            };
            match result {
                Ok(()) => {
                    let sample = std::mem::ManuallyDrop::into_inner(std::mem::replace(
                        &mut output.pSample,
                        std::mem::ManuallyDrop::new(None),
                    ));
                    if let Some(sample) = sample {
                        frames.push(self.to_frame(&sample, self.last_pts_ms.unwrap_or(0))?);
                    }
                }
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // Resolution/format change: renegotiate and keep draining.
                    self.select_output_type()?;
                }
                Err(error) => return Err(anyhow!("ProcessOutput: {error}")),
            }
        }
        Ok(frames)
    }

    fn to_frame(&self, sample: &IMFSample, fallback_pts_ms: u32) -> Result<DecodedFrame> {
        let pts_ms = unsafe {
            sample
                .GetSampleTime()
                .ok()
                .map(|ticks| (ticks / TICKS_PER_MS).max(0) as u32)
                .unwrap_or(fallback_pts_ms)
        };
        unsafe {
            let buffer = sample
                .ConvertToContiguousBuffer()
                .context("ConvertToContiguousBuffer")?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut pitch = 0i32;
                buffer2d.Lock2D(&mut ptr, &mut pitch).context("Lock2D")?;
                let frame = self.from_nv12(ptr, pitch as usize, pts_ms);
                buffer2d.Unlock2D().ok();
                return frame;
            }
            // Fallback: assume the row pitch equals the width.
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None).context("buffer lock")?;
            let frame = self.from_nv12(ptr, self.width as usize, pts_ms);
            buffer.Unlock().ok();
            frame
        }
    }

    fn from_nv12(&self, ptr: *const u8, stride: usize, pts_ms: u32) -> Result<DecodedFrame> {
        let w = self.width as usize;
        let h = self.height as usize;
        if w == 0 || h == 0 || ptr.is_null() {
            return Err(anyhow!("decoded frame before the output type was set"));
        }
        let y_len = stride * h;
        let uv_len = stride * h.div_ceil(2);
        // SAFETY: the MFT owns a buffer of at least this size in NV12 layout
        // while it is locked; we copy out before unlocking.
        let data = unsafe { std::slice::from_raw_parts(ptr, y_len + uv_len) };
        Ok(planes::from_nv12(
            self.width,
            self.height,
            &data[..y_len],
            stride,
            &data[y_len..],
            stride,
            pts_ms,
        ))
    }
}

impl VideoDecoder for MediaFoundationDecoder {
    fn decode(&mut self, packet: &[u8], keyframe: bool, pts_ms: u32) -> Result<Vec<DecodedFrame>> {
        ensure_platform();
        if !self.configured {
            if !keyframe {
                // Wait for a keyframe before we can configure the decoder.
                return Ok(Vec::new());
            }
            self.configure(packet)?;
        }
        self.push_sample(packet, pts_ms)?;
        self.drain()
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        if !self.configured {
            return Ok(Vec::new());
        }
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .ok();
        }
        self.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{ManuallyDrop, replace};

    /// `CLSID_CMSH264EncoderMFT` — lets the test produce real H.264 to decode.
    const CLSID_H264_ENCODER_MFT: GUID =
        GUID::from_u128(0x6CA5_0344_051A_4DED_9779_A433_0516_5E35);

    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 240;

    /// Media Foundation packs (numerator, denominator) / (width, height) into the
    /// high and low 32 bits of a UINT64.
    fn pack(high: u32, low: u32) -> u64 {
        ((high as u64) << 32) | low as u64
    }

    fn grey_nv12(shade: u8) -> Vec<u8> {
        let mut data = vec![shade; (WIDTH * HEIGHT) as usize];
        data.extend(std::iter::repeat(128u8).take((WIDTH * HEIGHT / 2) as usize));
        data
    }

    fn sample_from(bytes: &[u8], time: i64) -> IMFSample {
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes.len() as u32).unwrap();
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None).unwrap();
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            buffer.Unlock().ok();
            buffer.SetCurrentLength(bytes.len() as u32).unwrap();
            let sample = MFCreateSample().unwrap();
            sample.AddBuffer(&buffer).unwrap();
            sample.SetSampleTime(time).unwrap();
            sample.SetSampleDuration(333_333).unwrap();
            sample
        }
    }

    /// Encode a few NV12 frames and return Annex-B access units (with the
    /// encoder's sequence header prepended to the first).
    fn encode_frames(count: usize) -> Vec<Vec<u8>> {
        unsafe {
            let encoder: IMFTransform =
                CoCreateInstance(&CLSID_H264_ENCODER_MFT, None, CLSCTX_INPROC_SERVER)
                    .expect("create H.264 encoder MFT");

            let output = MFCreateMediaType().unwrap();
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).unwrap();
            output.SetUINT32(&MF_MT_AVG_BITRATE, 1_000_000).unwrap();
            output.SetUINT64(&MF_MT_FRAME_SIZE, pack(WIDTH, HEIGHT)).unwrap();
            output.SetUINT64(&MF_MT_FRAME_RATE, pack(30, 1)).unwrap();
            output.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).unwrap();
            output
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .unwrap();
            encoder.SetOutputType(0, Some(&output), 0).unwrap();

            let input = MFCreateMediaType().unwrap();
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).unwrap();
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(WIDTH, HEIGHT)).unwrap();
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(30, 1)).unwrap();
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).unwrap();
            input
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .unwrap();
            encoder.SetInputType(0, Some(&input), 0).unwrap();

            encoder
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .unwrap();
            encoder
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .unwrap();

            let out_info = encoder.GetOutputStreamInfo(0).unwrap();
            let provides_output =
                out_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            let out_size = out_info.cbSize.max(WIDTH * HEIGHT);

            let mut sequence_header: Vec<u8> = Vec::new();
            let mut access_units: Vec<Vec<u8>> = Vec::new();

            for index in 0..count {
                let frame = grey_nv12((index * 40) as u8);
                let sample = sample_from(&frame, index as i64 * 333_333);
                encoder.ProcessInput(0, Some(&sample), 0).unwrap();

                loop {
                    let mut out = MFT_OUTPUT_DATA_BUFFER::default();
                    if !provides_output {
                        let buffer = MFCreateMemoryBuffer(out_size).unwrap();
                        let out_sample = MFCreateSample().unwrap();
                        out_sample.AddBuffer(&buffer).unwrap();
                        out.pSample = ManuallyDrop::new(Some(out_sample));
                    }
                    let mut status = 0u32;
                    match encoder.ProcessOutput(0, std::slice::from_mut(&mut out), &mut status) {
                        Ok(()) => {
                            let sample = ManuallyDrop::into_inner(replace(
                                &mut out.pSample,
                                ManuallyDrop::new(None),
                            ));
                            if let Some(sample) = sample {
                                access_units.push(contiguous_bytes(&sample));
                            }
                        }
                        Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                            // The encoder has (re)negotiated its output type; the
                            // new type carries the sequence header.
                            let current = encoder.GetOutputCurrentType(0).unwrap();
                            let size = current.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).unwrap_or(0);
                            if size > 0 {
                                let mut blob = vec![0u8; size as usize];
                                current
                                    .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None)
                                    .unwrap();
                                sequence_header = blob;
                            }
                            encoder.SetOutputType(0, Some(&current), 0).unwrap();
                        }
                        Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                        Err(error) => panic!("encoder ProcessOutput: {error}"),
                    }
                }
            }
            encoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).ok();
            loop {
                let mut out = MFT_OUTPUT_DATA_BUFFER::default();
                if !provides_output {
                    let buffer = MFCreateMemoryBuffer(out_size).unwrap();
                    let out_sample = MFCreateSample().unwrap();
                    out_sample.AddBuffer(&buffer).unwrap();
                    out.pSample = ManuallyDrop::new(Some(out_sample));
                }
                let mut status = 0u32;
                match encoder.ProcessOutput(0, std::slice::from_mut(&mut out), &mut status) {
                    Ok(()) => {
                        let sample = ManuallyDrop::into_inner(replace(
                            &mut out.pSample,
                            ManuallyDrop::new(None),
                        ));
                        if let Some(sample) = sample {
                            access_units.push(contiguous_bytes(&sample));
                        }
                    }
                    Err(_) => break,
                }
            }

            if !sequence_header.is_empty() {
                if let Some(first) = access_units.first_mut() {
                    let mut with_header = sequence_header.clone();
                    with_header.extend_from_slice(first);
                    *first = with_header;
                }
            }
            access_units
        }
    }

    fn contiguous_bytes(sample: &IMFSample) -> Vec<u8> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().unwrap();
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len)).unwrap();
            let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock().ok();
            bytes
        }
    }

    #[test]
    fn encodes_then_decodes_a_frame() {
        ensure_platform();
        let access_units = encode_frames(6);
        assert!(!access_units.is_empty(), "encoder produced no access units");

        let mut decoder = MediaFoundationDecoder::new(Codec::H264).unwrap();
        let mut decoded = Vec::new();
        for (index, au) in access_units.iter().enumerate() {
            decoded.extend(decoder.decode(au, index == 0, index as u32 * 33).unwrap());
        }
        decoded.extend(decoder.flush().unwrap());

        let frame = decoded.first().expect("no frame decoded");
        assert_eq!(frame.width, WIDTH);
        assert_eq!(frame.height, HEIGHT);
        assert_eq!(frame.planes.len(), 3);
        assert_eq!(frame.planes[0].data.len(), (WIDTH * HEIGHT) as usize);
    }
}
