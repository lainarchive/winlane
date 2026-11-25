use crate::error::{ViewError, ViewResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoomViewport {
    zoom_percent: u16,
    center_x: u32,
    center_y: u32,
    initialized: bool,
}

impl ZoomViewport {
    pub fn new(zoom_percent: u16) -> ViewResult<Self> {
        if !(100..=400).contains(&zoom_percent) {
            return Err(ViewError::InvalidZoom { zoom_percent });
        }
        Ok(Self {
            zoom_percent,
            center_x: 0,
            center_y: 0,
            initialized: false,
        })
    }

    pub fn zoom_percent(&self) -> u16 {
        self.zoom_percent
    }

    pub fn rect_for(&mut self, frame_width: u32, frame_height: u32) -> ViewResult<SourceRect> {
        if frame_width == 0 || frame_height == 0 {
            return Err(ViewError::EmptyFrame);
        }
        if !self.initialized {
            self.center_x = frame_width / 2;
            self.center_y = frame_height / 2;
            self.initialized = true;
        }
        let width = crop_dimension(frame_width, self.zoom_percent);
        let height = crop_dimension(frame_height, self.zoom_percent);
        self.center_x = clamp_center(self.center_x, width, frame_width);
        self.center_y = clamp_center(self.center_y, height, frame_height);
        Ok(SourceRect {
            x: clamped_left(self.center_x, width, frame_width),
            y: clamped_left(self.center_y, height, frame_height),
            width,
            height,
        })
    }

    pub fn pan_by(
        &mut self,
        dx: i32,
        dy: i32,
        frame_width: u32,
        frame_height: u32,
    ) -> ViewResult<()> {
        let crop = self.rect_for(frame_width, frame_height)?;
        self.center_x = pan_coordinate(self.center_x, dx, crop.width, frame_width);
        self.center_y = pan_coordinate(self.center_y, dy, crop.height, frame_height);
        Ok(())
    }
}

fn crop_dimension(source: u32, zoom_percent: u16) -> u32 {
    if zoom_percent == 100 {
        return source;
    }
    ((u64::from(source) * 100) / u64::from(zoom_percent))
        .max(1)
        .min(u64::from(source)) as u32
}

fn clamped_left(center: u32, crop: u32, source: u32) -> u32 {
    if crop >= source {
        return 0;
    }
    center
        .saturating_sub(crop / 2)
        .min(source.saturating_sub(crop))
}

fn clamp_center(center: u32, crop: u32, source: u32) -> u32 {
    let (min, max) = center_range(crop, source);
    center.clamp(min, max)
}

fn pan_coordinate(center: u32, delta: i32, crop: u32, source: u32) -> u32 {
    let (min, max) = center_range(crop, source);
    (i64::from(center) + i64::from(delta)).clamp(i64::from(min), i64::from(max)) as u32
}

fn center_range(crop: u32, source: u32) -> (u32, u32) {
    if crop >= source {
        let center = source / 2;
        return (center, center);
    }
    let leading = crop / 2;
    (leading, source - crop + leading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_100_uses_entire_frame() {
        let mut viewport = ZoomViewport::new(100).unwrap();
        assert_eq!(
            viewport.rect_for(1024, 768).unwrap(),
            SourceRect {
                x: 0,
                y: 0,
                width: 1024,
                height: 768,
            }
        );
    }

    #[test]
    fn zoom_200_center_crops_half_dimensions() {
        let mut viewport = ZoomViewport::new(200).unwrap();
        assert_eq!(
            viewport.rect_for(1024, 768).unwrap(),
            SourceRect {
                x: 256,
                y: 192,
                width: 512,
                height: 384,
            }
        );
    }

    #[test]
    fn panning_moves_crop_and_clamps_at_edges() {
        let mut viewport = ZoomViewport::new(200).unwrap();
        let _ = viewport.rect_for(1024, 768).unwrap();
        viewport.pan_by(200, 100, 1024, 768).unwrap();
        assert_eq!(viewport.rect_for(1024, 768).unwrap().x, 456);
        assert_eq!(viewport.rect_for(1024, 768).unwrap().y, 292);
        viewport.pan_by(10_000, 10_000, 1024, 768).unwrap();
        let rect = viewport.rect_for(1024, 768).unwrap();
        assert_eq!(rect.x + rect.width, 1024);
        assert_eq!(rect.y + rect.height, 768);
    }

    #[test]
    fn reverse_pan_moves_immediately_after_edge_clamp() {
        let mut viewport = ZoomViewport::new(200).unwrap();
        let _ = viewport.rect_for(1024, 768).unwrap();
        viewport.pan_by(10_000, 0, 1024, 768).unwrap();
        assert_eq!(viewport.rect_for(1024, 768).unwrap().x, 512);
        viewport.pan_by(-128, 0, 1024, 768).unwrap();
        assert_eq!(viewport.rect_for(1024, 768).unwrap().x, 384);
    }

    #[test]
    fn invalid_zoom_is_rejected() {
        assert!(matches!(
            ZoomViewport::new(99),
            Err(ViewError::InvalidZoom { .. })
        ));
        assert!(matches!(
            ZoomViewport::new(401),
            Err(ViewError::InvalidZoom { .. })
        ));
    }
}
