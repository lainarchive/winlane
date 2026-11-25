use std::cell::RefCell;

use crate::error::{ViewError, ViewResult};
use crate::viewport::SourceRect;

pub const MAX_FRAME_PIXELS: usize = 33_177_600;

std::thread_local! {
    static CATMULL_ROM_RESIZER: RefCell<fast_image_resize::Resizer> =
        RefCell::new(fast_image_resize::Resizer::new());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb8 {
    pub const BLACK: Self = Self::new(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<Rgb8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, pixels: Vec<Rgb8>) -> ViewResult<Self> {
        if width == 0 || height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        let expected = width as usize * height as usize;
        if expected > MAX_FRAME_PIXELS {
            return Err(ViewError::FrameTooLarge {
                pixels: expected,
                maximum: MAX_FRAME_PIXELS,
            });
        }
        if pixels.len() != expected {
            return Err(ViewError::PixelLengthMismatch {
                width,
                height,
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn solid(width: u32, height: u32, color: Rgb8) -> ViewResult<Self> {
        if width == 0 || height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        let len = width as usize * height as usize;
        if len > MAX_FRAME_PIXELS {
            return Err(ViewError::FrameTooLarge {
                pixels: len,
                maximum: MAX_FRAME_PIXELS,
            });
        }
        Self::new(width, height, vec![color; len])
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[Rgb8] {
        &self.pixels
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels
            .get(y as usize * self.width as usize + x as usize)
            .copied()
    }

    pub fn crop(&self, rect: SourceRect) -> ViewResult<Self> {
        if rect.width == 0 || rect.height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        if rect.x >= self.width
            || rect.y >= self.height
            || rect.width > self.width - rect.x
            || rect.height > self.height - rect.y
        {
            return Err(ViewError::CropOutOfBounds);
        }
        let mut pixels = Vec::with_capacity(rect.width as usize * rect.height as usize);
        for y in rect.y..rect.y + rect.height {
            let start = (y * self.width + rect.x) as usize;
            let end = start + rect.width as usize;
            pixels.extend_from_slice(&self.pixels[start..end]);
        }
        Self::new(rect.width, rect.height, pixels)
    }

    pub fn rgb_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.pixels.len() * 3);
        for pixel in &self.pixels {
            bytes.extend_from_slice(&[pixel.r, pixel.g, pixel.b]);
        }
        bytes
    }

    pub fn scaled_nearest(&self, width: u32, height: u32) -> ViewResult<Self> {
        if width == self.width && height == self.height {
            return Ok(self.clone());
        }
        if width == 0 || height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        let len = width as usize * height as usize;
        if len > MAX_FRAME_PIXELS {
            return Err(ViewError::FrameTooLarge {
                pixels: len,
                maximum: MAX_FRAME_PIXELS,
            });
        }

        let mut pixels = Vec::with_capacity(len);
        for y in 0..height {
            let source_y = (u64::from(y) * u64::from(self.height) / u64::from(height)) as u32;
            for x in 0..width {
                let source_x = (u64::from(x) * u64::from(self.width) / u64::from(width)) as u32;
                if let Some(pixel) = self.pixel(source_x, source_y) {
                    pixels.push(pixel);
                }
            }
        }
        Self::new(width, height, pixels)
    }

    pub fn scaled_catmull_rom_rgb_bytes(&self, width: u32, height: u32) -> ViewResult<Vec<u8>> {
        if width == self.width && height == self.height {
            return Ok(self.rgb_bytes());
        }
        if width == 0 || height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        let len = width as usize * height as usize;
        if len > MAX_FRAME_PIXELS {
            return Err(ViewError::FrameTooLarge {
                pixels: len,
                maximum: MAX_FRAME_PIXELS,
            });
        }

        let source_bytes = self.rgb_bytes();
        let source = fast_image_resize::images::ImageRef::new(
            self.width,
            self.height,
            &source_bytes,
            fast_image_resize::PixelType::U8x3,
        )?;
        let mut destination = fast_image_resize::images::Image::new(
            width,
            height,
            fast_image_resize::PixelType::U8x3,
        );
        let options = fast_image_resize::ResizeOptions::new().resize_alg(
            fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::CatmullRom),
        );
        CATMULL_ROM_RESIZER
            .with_borrow_mut(|resizer| resizer.resize(&source, &mut destination, &options))?;
        Ok(destination.into_vec())
    }

    pub fn scaled_catmull_rom(&self, width: u32, height: u32) -> ViewResult<Self> {
        if width == self.width && height == self.height {
            return Ok(self.clone());
        }
        if width == 0 || height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        let len = width as usize * height as usize;
        let bytes = self.scaled_catmull_rom_rgb_bytes(width, height)?;
        let mut pixels = Vec::with_capacity(len);
        for channel in bytes.as_chunks::<3>().0 {
            pixels.push(Rgb8::new(channel[0], channel[1], channel[2]));
        }
        Self::new(width, height, pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mismatched_pixel_count() {
        let result = Frame::new(2, 2, vec![Rgb8::BLACK]);
        assert!(matches!(result, Err(ViewError::PixelLengthMismatch { .. })));
    }

    #[test]
    fn nearest_scaling_preserves_source_colors() {
        let frame = Frame::new(2, 1, vec![Rgb8::new(255, 0, 0), Rgb8::new(0, 0, 255)]).unwrap();
        let scaled = frame.scaled_nearest(4, 1).unwrap();
        assert_eq!(scaled.pixel(0, 0), Some(Rgb8::new(255, 0, 0)));
        assert_eq!(scaled.pixel(1, 0), Some(Rgb8::new(255, 0, 0)));
        assert_eq!(scaled.pixel(2, 0), Some(Rgb8::new(0, 0, 255)));
        assert_eq!(scaled.pixel(3, 0), Some(Rgb8::new(0, 0, 255)));
    }

    #[test]
    fn catmull_rom_scaling_supports_odd_and_tiny_dimensions() {
        let frame = Frame::new(
            5,
            7,
            (0..35)
                .map(|index| Rgb8::new(index as u8, (index * 3) as u8, (index * 7) as u8))
                .collect(),
        )
        .unwrap();
        for (width, height) in [(1, 1), (3, 4), (5, 1), (1, 7), (4, 3), (7, 5)] {
            let scaled = frame.scaled_catmull_rom(width, height).unwrap();
            assert_eq!((scaled.width(), scaled.height()), (width, height));
        }
    }

    #[test]
    fn catmull_rom_exact_scale_returns_native_pixels() {
        let frame = Frame::new(
            3,
            2,
            (0..6)
                .map(|index| Rgb8::new(index as u8, (index * 5) as u8, (index * 11) as u8))
                .collect(),
        )
        .unwrap();
        let scaled = frame.scaled_catmull_rom(3, 2).unwrap();
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(scaled.pixel(x, y), frame.pixel(x, y));
            }
        }
    }

    #[test]
    fn catmull_rom_rgb_bytes_have_exact_dimensions_and_native_exact_scale() {
        let frame = Frame::new(
            3,
            2,
            (0..6)
                .map(|index| Rgb8::new(index as u8, (index * 5) as u8, (index * 11) as u8))
                .collect(),
        )
        .unwrap();
        assert_eq!(
            frame.scaled_catmull_rom_rgb_bytes(3, 2).unwrap(),
            frame.rgb_bytes()
        );
        let scaled = frame.scaled_catmull_rom_rgb_bytes(5, 3).unwrap();
        assert_eq!(scaled.len(), 5 * 3 * 3);
        let scaled_frame = frame.scaled_catmull_rom(5, 3).unwrap();
        assert_eq!(scaled, scaled_frame.rgb_bytes());
    }

    #[test]
    fn crop_preserves_native_pixels_and_bounds() {
        let frame = Frame::new(
            3,
            2,
            vec![
                Rgb8::new(1, 0, 0),
                Rgb8::new(2, 0, 0),
                Rgb8::new(3, 0, 0),
                Rgb8::new(4, 0, 0),
                Rgb8::new(5, 0, 0),
                Rgb8::new(6, 0, 0),
            ],
        )
        .unwrap();
        let crop = frame
            .crop(SourceRect {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            })
            .unwrap();
        assert_eq!(crop.pixel(0, 0), Some(Rgb8::new(2, 0, 0)));
        assert_eq!(crop.pixel(1, 1), Some(Rgb8::new(6, 0, 0)));
        assert!(matches!(
            frame.crop(SourceRect {
                x: 2,
                y: 0,
                width: 2,
                height: 1,
            }),
            Err(ViewError::CropOutOfBounds)
        ));
    }
}
