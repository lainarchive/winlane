use crate::detect::GraphicsProtocol;
use crate::error::ViewResult;
use crate::frame::{Frame, Rgb8};
use crate::geometry::TerminalGeometry;
use crate::render::{RenderedOutput, render_frame};

pub fn moving_box_frame(width: u32, height: u32, step: u32) -> ViewResult<Frame> {
    let frame = Frame::solid(width, height, Rgb8::new(8, 12, 20))?;
    let box_size = (width.min(height) / 4).max(1);
    let travel = width.saturating_sub(box_size).max(1);
    let x0 = if travel == 0 { 0 } else { step % (travel + 1) };
    let y0 = height / 2;

    let mut pixels = frame.pixels().to_vec();
    for y in y0..(y0 + box_size).min(height) {
        for x in x0..(x0 + box_size).min(width) {
            let index = y as usize * width as usize + x as usize;
            if let Some(pixel) = pixels.get_mut(index) {
                *pixel = Rgb8::new(240, 96, 64);
            }
        }
    }
    Frame::new(width, height, pixels)
}

pub fn repaint_sequence(
    frame_width: u32,
    frame_height: u32,
    geometry: TerminalGeometry,
    protocol: GraphicsProtocol,
    steps: u32,
) -> ViewResult<Vec<RenderedOutput>> {
    let mut outputs = Vec::new();
    for step in 0..steps {
        let frame = moving_box_frame(frame_width, frame_height, step)?;
        outputs.push(render_frame(&frame, geometry, protocol)?);
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_repaints_have_same_dimensions_and_different_bytes() {
        let geometry = TerminalGeometry::cells(8, 4).unwrap();
        let outputs = repaint_sequence(16, 8, geometry, GraphicsProtocol::HalfBlock, 2).unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].plan, outputs[1].plan);
        assert_ne!(outputs[0].bytes, outputs[1].bytes);
    }

    #[test]
    fn kitty_repaint_harness_uses_graphics_protocol() {
        let geometry = TerminalGeometry::cells(8, 4).unwrap();
        let outputs = repaint_sequence(16, 8, geometry, GraphicsProtocol::Kitty, 1).unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].bytes.windows(3).any(|bytes| bytes == b"\x1b_G"));
    }
}
