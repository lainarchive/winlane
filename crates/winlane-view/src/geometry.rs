use crate::error::{ViewError, ViewResult};
use crate::frame::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalGeometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

impl TerminalGeometry {
    pub fn new(cols: u16, rows: u16, cell_width_px: u16, cell_height_px: u16) -> ViewResult<Self> {
        if cols == 0 || rows == 0 || cell_width_px == 0 || cell_height_px == 0 {
            return Err(ViewError::InvalidGeometry);
        }
        Ok(Self {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        })
    }

    pub fn cells(cols: u16, rows: u16) -> ViewResult<Self> {
        Self::new(cols, rows, 8, 16)
    }

    pub fn resized(&self, cols: u16, rows: u16) -> ViewResult<Self> {
        Self::new(cols, rows, self.cell_width_px, self.cell_height_px)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSurface {
    HalfBlockCells,
    KittyPixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPlan {
    pub surface: OutputSurface,
    pub surface_width: u32,
    pub surface_height: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub offset_x: u32,
    pub offset_y: u32,
}

impl RenderPlan {
    pub fn letterboxed(&self) -> bool {
        self.offset_x != 0 || self.offset_y != 0
    }
}

pub fn plan_render(
    frame_width: u32,
    frame_height: u32,
    geometry: TerminalGeometry,
    surface: OutputSurface,
) -> ViewResult<RenderPlan> {
    if frame_width == 0 || frame_height == 0 {
        return Err(ViewError::EmptyFrame);
    }
    let (surface_width, surface_height) = match surface {
        OutputSurface::HalfBlockCells => (u32::from(geometry.cols), u32::from(geometry.rows) * 2),
        OutputSurface::KittyPixels => (
            u32::from(geometry.cols) * u32::from(geometry.cell_width_px),
            u32::from(geometry.rows) * u32::from(geometry.cell_height_px),
        ),
    };

    let width_constrained = u64::from(surface_width) * u64::from(frame_height)
        <= u64::from(surface_height) * u64::from(frame_width);
    let (scaled_width, scaled_height) = if width_constrained {
        let width = surface_width;
        let height =
            ((u64::from(frame_height) * u64::from(width)) / u64::from(frame_width)).max(1) as u32;
        (width, height.min(surface_height))
    } else {
        let height = surface_height;
        let width =
            ((u64::from(frame_width) * u64::from(height)) / u64::from(frame_height)).max(1) as u32;
        (width.min(surface_width), height)
    };

    Ok(RenderPlan {
        surface,
        surface_width,
        surface_height,
        frame_width: scaled_width,
        frame_height: scaled_height,
        offset_x: (surface_width - scaled_width) / 2,
        offset_y: (surface_height - scaled_height) / 2,
    })
}

pub fn plan_frame(
    frame: &Frame,
    geometry: TerminalGeometry,
    surface: OutputSurface,
) -> ViewResult<RenderPlan> {
    plan_render(frame.width(), frame.height(), geometry, surface)
}

pub fn terminal_point_to_frame(
    plan: RenderPlan,
    source_width: u32,
    source_height: u32,
    col: u16,
    row: u16,
    geometry: TerminalGeometry,
) -> Option<(u32, u32)> {
    if source_width == 0 || source_height == 0 {
        return None;
    }
    let (x, y) = match plan.surface {
        OutputSurface::HalfBlockCells => (u32::from(col), u32::from(row) * 2 + 1),
        OutputSurface::KittyPixels => (
            u32::from(col) * u32::from(geometry.cell_width_px)
                + u32::from(geometry.cell_width_px) / 2,
            u32::from(row) * u32::from(geometry.cell_height_px)
                + u32::from(geometry.cell_height_px) / 2,
        ),
    };

    let x = x.checked_sub(plan.offset_x)?;
    let y = y.checked_sub(plan.offset_y)?;
    if x >= plan.frame_width || y >= plan.frame_height {
        return None;
    }

    let source_x = (u64::from(x) * u64::from(source_width) / u64::from(plan.frame_width)) as u32;
    let source_y = (u64::from(y) * u64::from(source_height) / u64::from(plan.frame_height)) as u32;
    Some((source_x, source_y))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resize {
    pub before: TerminalGeometry,
    pub after: TerminalGeometry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportState {
    geometry: TerminalGeometry,
    full_repaint_required: bool,
}

impl ViewportState {
    pub fn new(geometry: TerminalGeometry) -> Self {
        Self {
            geometry,
            full_repaint_required: true,
        }
    }

    pub fn geometry(&self) -> TerminalGeometry {
        self.geometry
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> ViewResult<Option<Resize>> {
        let after = self.geometry.resized(cols, rows)?;
        if after == self.geometry {
            return Ok(None);
        }
        let before = self.geometry;
        self.geometry = after;
        self.full_repaint_required = true;
        Ok(Some(Resize { before, after }))
    }

    pub fn full_repaint_required(&self) -> bool {
        self.full_repaint_required
    }

    pub fn repaint_completed(&mut self) {
        self.full_repaint_required = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterboxes_wide_frame_exactly() {
        let geometry = TerminalGeometry::cells(20, 10).unwrap();
        let plan = plan_render(40, 10, geometry, OutputSurface::HalfBlockCells).unwrap();
        assert_eq!(plan.surface_width, 20);
        assert_eq!(plan.surface_height, 20);
        assert_eq!(plan.frame_width, 20);
        assert_eq!(plan.frame_height, 5);
        assert_eq!(plan.offset_x, 0);
        assert_eq!(plan.offset_y, 7);
    }

    #[test]
    fn letterboxes_tall_frame_exactly() {
        let geometry = TerminalGeometry::new(10, 10, 8, 16).unwrap();
        let plan = plan_render(10, 40, geometry, OutputSurface::KittyPixels).unwrap();
        assert_eq!(plan.surface_width, 80);
        assert_eq!(plan.surface_height, 160);
        assert_eq!(plan.frame_width, 40);
        assert_eq!(plan.frame_height, 160);
        assert_eq!(plan.offset_x, 20);
        assert_eq!(plan.offset_y, 0);
    }

    #[test]
    fn resize_requires_repaint_only_on_dimension_change() {
        let geometry = TerminalGeometry::cells(20, 10).unwrap();
        let mut state = ViewportState::new(geometry);
        state.repaint_completed();
        assert!(state.resize(20, 10).unwrap().is_none());
        assert!(!state.full_repaint_required());
        assert!(state.resize(30, 10).unwrap().is_some());
        assert!(state.full_repaint_required());
    }

    #[test]
    fn zero_terminal_or_cell_dimensions_are_rejected() {
        for (cols, rows, cell_width, cell_height) in
            [(0, 1, 1, 1), (1, 0, 1, 1), (1, 1, 0, 1), (1, 1, 1, 0)]
        {
            assert!(matches!(
                TerminalGeometry::new(cols, rows, cell_width, cell_height),
                Err(ViewError::InvalidGeometry)
            ));
        }
    }

    #[test]
    fn invalid_resize_does_not_mutate_or_require_repaint() {
        let geometry = TerminalGeometry::cells(20, 10).unwrap();
        let mut state = ViewportState::new(geometry);
        state.repaint_completed();
        assert!(matches!(
            state.resize(0, 10),
            Err(ViewError::InvalidGeometry)
        ));
        assert_eq!(state.geometry(), geometry);
        assert!(!state.full_repaint_required());
    }

    #[test]
    fn extreme_aspect_ratios_remain_inside_surface() {
        let geometry = TerminalGeometry::cells(2, 2).unwrap();
        for (width, height) in [(10_000, 1), (1, 10_000)] {
            for surface in [OutputSurface::HalfBlockCells, OutputSurface::KittyPixels] {
                let plan = plan_render(width, height, geometry, surface).unwrap();
                assert!(plan.frame_width >= 1 && plan.frame_width <= plan.surface_width);
                assert!(plan.frame_height >= 1 && plan.frame_height <= plan.surface_height);
                assert!(plan.offset_x + plan.frame_width <= plan.surface_width);
                assert!(plan.offset_y + plan.frame_height <= plan.surface_height);
            }
        }
    }
}
