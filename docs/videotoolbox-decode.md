# Native VideoToolbox decoding (macOS client)

Replaces the FFmpeg decoder on macOS with the system `VTDecompressionSession`.
No external packages, hardware accelerated, and it removes the last reason to
install FFmpeg on an Intel Mac where Homebrew compiles everything from source.

This is **client-side only**; the macOS *host* already encodes with
VideoToolbox.

## Where it plugs in

- Implement `macss_decoder::VideoDecoder` in a new module
  `client/crates/decoder/src/videotoolbox.rs`, gated
  `#[cfg(all(feature = "videotoolbox", target_os = "macos"))]`.
- Return it from `macss_decoder::default_decoder()` when that cfg matches;
  otherwise keep the existing FFmpeg/Null behaviour so Windows is unaffected.
- Also extend `macss_decoder::backend_name()`.

Helpers already exist and are unit-tested (verified on Windows):

| Helper | Purpose |
|---|---|
| `annexb::split_nals` | Annex-B → NAL units |
| `annexb::split_parameter_sets` | separate SPS/PPS(/VPS) from coded slices |
| `annexb::to_avcc` | length-prefixed access unit for `CMSampleBuffer` |
| `planes::from_nv12` | `CVPixelBuffer` planes → `DecodedFrame` (YUV420p) |
| `planes::from_planar` | same, for planar sources |

## Cargo changes

Verified against the published crates (`objc2-* 0.3.2`, `block2 0.6.2`). These
are macOS-only, so Windows is unaffected:

```toml
[features]
videotoolbox = [
    "dep:block2",
    "dep:objc2-core-foundation",
    "dep:objc2-core-media",
    "dep:objc2-core-video",
    "dep:objc2-video-toolbox",
]

[target.'cfg(target_os = "macos")'.dependencies]
block2 = { version = "0.6", optional = true }
objc2-core-foundation = { version = "0.3", optional = true, default-features = false,
    features = ["CFAllocator", "CFDictionary", "CFNumber", "CFCGTypes"] }
objc2-core-media = { version = "0.3", optional = true, default-features = false,
    features = ["alloc", "CMBase", "CMBlockBuffer", "CMFormatDescription", "CMSampleBuffer", "CMTime"] }
objc2-core-video = { version = "0.3", optional = true, default-features = false,
    features = ["CVBuffer", "CVImageBuffer", "CVPixelBuffer"] }
objc2-video-toolbox = { version = "0.3", optional = true, default-features = false,
    features = ["VTDecompressionSession", "VTDecompressionProperties"] }
```

The framework crates are thin, generated bindings over the C API: names match
the C functions, everything is `unsafe`, and pointers are raw. Prefer the
type-safe inherent methods over the free functions — the free functions are
`#[deprecated]` aliases.

## Exact API surface (verified)

```rust
// objc2_video_toolbox
unsafe fn VTDecompressionSession::create(
    allocator: Option<&CFAllocator>,
    video_format_description: &CMVideoFormatDescription,
    video_decoder_specification: Option<&CFDictionary>,
    destination_image_buffer_attributes: Option<&CFDictionary>,
    output_callback: *const VTDecompressionOutputCallbackRecord, // null
    decompression_session_out: NonNull<*mut VTDecompressionSession>,
) -> OSStatus;

unsafe fn VTDecompressionSession::decode_frame_with_output_handler(
    &self,
    sample_buffer: &CMSampleBuffer,
    decode_flags: VTDecodeFrameFlags,
    info_flags_out: *mut VTDecodeInfoFlags,
    output_handler: VTDecompressionOutputHandler,
) -> OSStatus;

unsafe fn VTDecompressionSession::wait_for_asynchronous_frames(&self) -> OSStatus;

// The handler is a block, hence the block2 dependency:
type VTDecompressionOutputHandler =
    *mut block2::DynBlock<dyn Fn(OSStatus, VTDecodeInfoFlags, *mut CVImageBuffer, CMTime, CMTime)>;

// objc2_core_media
unsafe fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
    allocator: Option<&CFAllocator>,
    parameter_set_count: usize,
    parameter_set_pointers: NonNull<NonNull<u8>>,
    parameter_set_sizes: NonNull<usize>,
    nal_unit_header_length: c_int,          // 4
    format_description_out: NonNull<*const CMFormatDescription>,
) -> OSStatus;
// ...CreateFromHEVCParameterSets has the same shape.

unsafe fn CMSampleBufferCreateReady(
    allocator: Option<&CFAllocator>,
    data_buffer: Option<&CMBlockBuffer>,
    format_description: Option<&CMFormatDescription>,
    num_samples: CMItemCount,               // 1
    num_sample_timing_entries: CMItemCount, // 0
    sample_timing_array: *const CMSampleTimingInfo, // null
    num_sample_size_entries: CMItemCount,   // 0
    sample_size_array: *const usize,        // null
    sample_buffer_out: NonNull<*mut CMSampleBuffer>,
) -> OSStatus;
```

Use `block2::RcBlock::new(move |status, flags, image_buffer, pts, dur| { ... })`
and pass `RcBlock::as_ptr(...)` as the handler.

## Decode pipeline

The host guarantees **chunk 0 of every keyframe starts with the parameter sets**
(see `docs/protocol.md` §3), so the decoder is self-configuring.

```
keyframe  ──► split_nals ──► split_parameter_sets ──► format description (once)
                             └────────────────────► slices ──► to_avcc ──► sample buffer
any frame ──► slices ──► to_avcc ──► CMSampleBuffer ──► decode_frame_with_output_handler
                                                          └─► CVPixelBuffer ──► planes::from_nv12
```

1. **Format description**, created on each keyframe from the parameter sets
   (without start codes) and `nal_unit_header_length: 4`. Recreate the session
   only if the sets actually change; rebuilding the description per keyframe is
   cheap and fine.
2. **Session**, created lazily on the first keyframe:
   - `video_decoder_specification`:
     `{ kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true }`
   - `destination_image_buffer_attributes`:
     `{ kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange }`
   - then `kVTDecompressionPropertyKey_RealTime = true`.
   - `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` is NV12, **video range** —
     exactly what `shader.wgsl` assumes (BT.709 limited).
3. **Per frame**: `to_avcc(&slices)` → `CMBlockBuffer` → `CMSampleBufferCreateReady`
   → decode. Keep the block buffer alive for the duration of the decode call.
4. **In the handler**: `*mut CVImageBuffer` → treat as `CVPixelBuffer`, lock,
   read plane 0 (Y) and plane 1 (interleaved CbCr) with
   `CVPixelBufferGetBaseAddressOfPlane` / `CVPixelBufferGetBytesPerRowOfPlane`,
   call `planes::from_nv12`, unlock.
5. **`flush()`**: `wait_for_asynchronous_frames` and return anything buffered.

VideoToolbox is asynchronous. Start with the simplest correct thing: block on the
handler (a `OnceLock`/`mpsc`) and return the frame, rather than trying to stream
out-of-band.

## Gotchas

- **No frames out for some inputs.** VT buffers until it has enough data, so
  `decode` legitimately returns an empty `Vec`. The trait already allows that.
- **P-frame before the first keyframe** → no format description → skip and wait.
  The host sends an IDR on start and every ~2 s.
- **Copy out of the pixel buffer before the handler returns** — don't hold the
  lock or the buffer past it (`planes::from_nv12` copies).
- **`decode_frame_with_output_handler` is `unsafe`** and the handler is a raw
  block pointer: keep the `RcBlock` alive until the call returns.
- Decoding needs **no** TCC permissions (unlike the host's capture).
- If NV12 ever misbehaves, `kCVPixelFormatType_32BGRA` works, but you'd need a
  second shader path — avoid it.

## Acceptance criteria

1. `cargo build -p macss-client --features macss-decoder/videotoolbox` on macOS
   with no Homebrew packages installed.
2. `cargo run -p macss-client -- 127.0.0.1` against the local host shows the
   Mac's screen, letterboxed, at a steady frame rate.
3. `--headless` reports "decoded" counts climbing (proof it is decoding, not just
   counting packets).
4. `cargo test --workspace` still passes on Windows and macOS — the new module
   is cfg'd out everywhere else.
5. Mouse/keyboard passthrough still works in the windowed client.

## Suggested order of work

1. Add the feature/deps and a module that constructs the session and logs the
   format description — prove the bindings link.
2. Decode one keyframe into a `CVPixelBuffer`; log dimensions and plane strides.
3. Wire `planes::from_nv12` through to `DecodedFrame` and render.
4. Handle P-frame-only inputs, `flush()`, and the "no frames yet" case.
