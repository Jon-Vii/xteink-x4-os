use crate::{FB_BYTES, HEIGHT, ROW_BYTES, WIDTH};

pub struct Framebuffer {
    data: [u8; FB_BYTES],
}

impl Framebuffer {
    pub const fn new() -> Self {
        Self {
            data: [0xFF; FB_BYTES],
        }
    }

    #[inline]
    pub fn bytes(&self) -> &[u8; FB_BYTES] {
        &self.data
    }

    pub fn clear(&mut self, white: bool) {
        self.data.fill(if white { 0xFF } else { 0x00 });
    }

    pub fn copy_from(&mut self, other: &Self) {
        self.data.copy_from_slice(other.bytes());
    }

    pub fn band(&self, y: usize, rows: usize) -> &[u8] {
        let start = y * ROW_BYTES;
        let end = start + rows.min(HEIGHT - y) * ROW_BYTES;
        &self.data[start..end]
    }

    #[inline]
    pub fn set_pixel(&mut self, x: usize, y: usize, white: bool) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }

        let index = y * ROW_BYTES + x / 8;
        let mask = 0x80 >> (x & 7);
        if white {
            self.data[index] |= mask;
        } else {
            self.data[index] &= !mask;
        }
    }

    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= WIDTH || y >= HEIGHT {
            return true;
        }

        let index = y * ROW_BYTES + x / 8;
        let mask = 0x80 >> (x & 7);
        self.data[index] & mask != 0
    }

    /// Mirror across the long axis: swap row `y` with row `HEIGHT - 1 - y`.
    /// In a row-major 1 bpp buffer this is pure row swapping, no bit work.
    pub fn flip_vertical(&mut self) {
        let (top, bottom) = self.data.split_at_mut(FB_BYTES / 2);
        for (top_row, bottom_row) in top
            .chunks_exact_mut(ROW_BYTES)
            .zip(bottom.chunks_exact_mut(ROW_BYTES).rev())
        {
            top_row.swap_with_slice(bottom_row);
        }
    }
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_vertical_mirrors_rows() {
        let mut fb = Framebuffer::new();
        fb.set_pixel(0, 0, false);
        fb.set_pixel(799, 0, false);
        fb.set_pixel(13, 7, false);
        fb.set_pixel(400, 239, false);
        fb.set_pixel(401, 240, false);

        let mut expected = Framebuffer::new();
        expected.set_pixel(0, HEIGHT - 1, false);
        expected.set_pixel(799, HEIGHT - 1, false);
        expected.set_pixel(13, HEIGHT - 1 - 7, false);
        expected.set_pixel(400, HEIGHT - 1 - 239, false);
        expected.set_pixel(401, HEIGHT - 1 - 240, false);

        fb.flip_vertical();
        assert_eq!(fb.bytes()[..], expected.bytes()[..]);
    }

    #[test]
    fn flip_vertical_twice_is_identity() {
        let mut fb = Framebuffer::new();
        for (i, x) in [3usize, 99, 200, 798].iter().enumerate() {
            fb.set_pixel(*x, i * 123 % HEIGHT, false);
        }
        let original = *fb.bytes();
        fb.flip_vertical();
        fb.flip_vertical();
        assert_eq!(fb.bytes()[..], original[..]);
    }
}
