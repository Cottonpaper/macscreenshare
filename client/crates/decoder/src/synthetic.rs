//! A moving test pattern used to exercise the render path when no real decoder
//! is available (for example before FFmpeg is installed).

use crate::{DecodedFrame, PixelFormat, Plane};

/// Generates YUV 4:2:0 frames with a scrolling gradient and a moving bar.
pub struct SyntheticSource {
    width: u32,
    height: u32,
    tick: u32,
}

impl SyntheticSource {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(2),
            height: height.max(2),
            tick: 0,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Produce the next frame and advance the animation.
    pub fn next_frame(&mut self) -> DecodedFrame {
        let w = self.width as usize;
        let h = self.height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);

        let mut y = vec![0u8; w * h];
        let bar = (self.tick as usize * 4) % w;

        for row in 0..h {
            let base = row * w;
            for col in 0..w {
                // Diagonal gradient plus a bright moving bar.
                let grad = ((col + row) / 2 + self.tick as usize) % 256;
                let value = if col.abs_diff(bar) < 3 {
                    (grad as usize / 3) as u8
                } else {
                    grad as u8
                };
                y[base + col] = value;
            }
        }

        let mut u = vec![128u8; cw * ch];
        let mut v = vec![128u8; cw * ch];
        for row in 0..ch {
            for col in 0..cw {
                let idx = row * cw + col;
                u[idx] = (96 + ((col * 2 + self.tick as usize) % 64)) as u8;
                v[idx] = (160 + ((row * 2) % 64)) as u8;
            }
        }

        self.tick = self.tick.wrapping_add(1);

        DecodedFrame {
            width: self.width,
            height: self.height,
            format: PixelFormat::Yuv420p,
            planes: vec![
                Plane { data: y, stride: w },
                Plane { data: u, stride: cw },
                Plane { data: v, stride: cw },
            ],
            pts_ms: self.tick.saturating_mul(16),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_consistent_plane_sizes() {
        let mut source = SyntheticSource::new(64, 48);
        let frame = source.next_frame();
        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 48);
        assert_eq!(frame.format, PixelFormat::Yuv420p);
        assert_eq!(frame.planes[0].data.len(), 64 * 48);
        assert_eq!(frame.planes[1].data.len(), 32 * 24);
        assert_eq!(frame.planes[2].data.len(), 32 * 24);
    }

    #[test]
    fn frames_change_over_time() {
        let mut source = SyntheticSource::new(32, 32);
        let a = source.next_frame();
        let b = source.next_frame();
        assert_ne!(a.planes[0].data, b.planes[0].data);
        assert_ne!(a.pts_ms, b.pts_ms);
    }

    #[test]
    fn handles_odd_dimensions() {
        let mut source = SyntheticSource::new(33, 17);
        let frame = source.next_frame();
        let cw = 33usize.div_ceil(2);
        let ch = 17usize.div_ceil(2);
        assert_eq!(frame.planes[1].data.len(), cw * ch);
        assert_eq!(frame.planes[2].stride, cw);
    }
}
