//! Building [`DecodedFrame`]s from raw pixel buffers.
//!
//! Native decoders hand back platform buffers (VideoToolbox `CVPixelBuffer`,
//! Media Foundation `IMFMediaBuffer`) that are usually NV12 with a row stride
//! wider than the image. These helpers handle the stride and de-interleaving so
//! every backend ends up producing the same planar YUV 4:2:0 frame the renderer
//! expects.

use crate::{DecodedFrame, PixelFormat, Plane};

/// Copy `rows` rows of `dst_stride` bytes, honouring a wider source stride.
fn copy_rows(src: &[u8], src_stride: usize, rows: usize, dst_stride: usize) -> Vec<u8> {
    let mut out = vec![0u8; dst_stride * rows];
    for row in 0..rows {
        let start = row * src_stride;
        if start >= src.len() {
            break;
        }
        let len = dst_stride.min(src.len() - start);
        out[row * dst_stride..row * dst_stride + len].copy_from_slice(&src[start..start + len]);
    }
    out
}

/// Build a planar YUV 4:2:0 frame from three (buffer, stride) planes.
pub fn from_planar(
    width: u32,
    height: u32,
    planes: [(&[u8], usize); 3],
    pts_ms: u32,
) -> DecodedFrame {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);

    let strides = [w, cw, cw];
    let rows = [h, ch, ch];

    let out = (0..3)
        .map(|index| {
            let (src, src_stride) = planes[index];
            Plane {
                data: copy_rows(src, src_stride, rows[index], strides[index]),
                stride: strides[index],
            }
        })
        .collect();

    DecodedFrame {
        width,
        height,
        format: PixelFormat::Yuv420p,
        planes: out,
        pts_ms,
    }
}

/// Build a planar YUV 4:2:0 frame from an NV12 buffer (Y plane plus interleaved
/// UV plane), splitting UV into separate U and V planes.
pub fn from_nv12(
    width: u32,
    height: u32,
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    pts_ms: u32,
) -> DecodedFrame {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);

    let luma = Plane {
        data: copy_rows(y, y_stride, h, w),
        stride: w,
    };

    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let base = row * uv_stride;
        for col in 0..cw {
            let index = base + col * 2;
            if index + 1 < uv.len() {
                u.push(uv[index]);
                v.push(uv[index + 1]);
            } else {
                // Missing chroma: neutral grey.
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
            luma,
            Plane { data: u, stride: cw },
            Plane { data: v, stride: cw },
        ],
        pts_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_copies_ignoring_extra_stride() {
        // 4x2 luma with two padding bytes per row (stride 6).
        let y = [1u8, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        // Chroma is 2x1 with stride 4.
        let u = [9u8, 10, 0, 0];
        let v = [11u8, 12, 0, 0];
        let frame = from_planar(4, 2, [(&y, 6), (&u, 4), (&v, 4)], 42);

        assert_eq!(frame.format, PixelFormat::Yuv420p);
        assert_eq!(frame.pts_ms, 42);
        assert_eq!(frame.planes[0].stride, 4);
        assert_eq!(frame.planes[0].data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(frame.planes[1].data, vec![9, 10]);
        assert_eq!(frame.planes[2].data, vec![11, 12]);
    }

    #[test]
    fn nv12_splits_interleaved_chroma() {
        // 4x2 image: chroma is 2x1, one row of U,V pairs.
        let y = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let uv = [100u8, 110, 120, 130];
        let frame = from_nv12(4, 2, &y, 4, &uv, 4, 7);

        assert_eq!(frame.planes[0].data, vec![10, 20, 30, 40, 50, 60, 70, 80]);
        assert_eq!(frame.planes[1].data, vec![100, 120]); // U
        assert_eq!(frame.planes[2].data, vec![110, 130]); // V
        assert_eq!(frame.planes[1].stride, 2);
    }

    #[test]
    fn handles_odd_dimensions() {
        // 3x3: chroma is 2x2.
        let y = vec![1u8; 3 * 3];
        let uv = vec![2u8; 2 * 4]; // stride 4, 2 rows
        let frame = from_nv12(3, 3, &y, 3, &uv, 4, 0);
        assert_eq!(frame.planes[0].data.len(), 9);
        assert_eq!(frame.planes[1].data.len(), 4);
        assert_eq!(frame.planes[2].data.len(), 4);
    }

    #[test]
    fn short_source_is_padded_not_panicking() {
        let y = [1u8, 2]; // only one row, but two expected
        let frame = from_planar(2, 2, [(&y, 2), (&[], 2), (&[], 2)], 0);
        assert_eq!(frame.planes[0].data, vec![1, 2, 0, 0]);
        assert_eq!(frame.planes[1].data.len(), 1);
    }

    #[test]
    fn nv12_missing_chroma_falls_back_to_grey() {
        let frame = from_nv12(2, 2, &[0u8; 4], 2, &[], 4, 0);
        assert_eq!(frame.planes[1].data, vec![128]);
        assert_eq!(frame.planes[2].data, vec![128]);
    }
}
