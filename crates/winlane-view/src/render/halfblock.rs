use crate::detect::GraphicsProtocol;
use crate::error::ViewResult;
use crate::frame::{Frame, Rgb8};
use crate::geometry::{OutputSurface, TerminalGeometry, plan_frame};
use crate::render::RenderedOutput;

pub fn render_halfblock(frame: &Frame, geometry: TerminalGeometry) -> ViewResult<RenderedOutput> {
    let plan = plan_frame(frame, geometry, OutputSurface::HalfBlockCells)?;
    let scaled = frame.scaled_nearest(plan.frame_width, plan.frame_height)?;
    let mut output =
        String::with_capacity(usize::from(geometry.cols) * usize::from(geometry.rows) * 42 + 16);
    output.push_str("\x1b[H");

    let mut foreground: Option<Rgb8> = None;
    let mut background: Option<Rgb8> = None;

    for row in 0..u32::from(geometry.rows) {
        for col in 0..u32::from(geometry.cols) {
            let top = pixel_at(&scaled, plan.offset_x, plan.offset_y, col, row * 2);
            let bottom = pixel_at(&scaled, plan.offset_x, plan.offset_y, col, row * 2 + 1);
            set_foreground(&mut output, &mut foreground, top);
            set_background(&mut output, &mut background, bottom);
            output.push('▀');
        }
        if row + 1 < u32::from(geometry.rows) {
            output.push_str("\r\n");
        }
    }

    output.push_str("\x1b[0m");
    Ok(RenderedOutput {
        protocol: GraphicsProtocol::HalfBlock,
        plan,
        bytes: output.into_bytes(),
    })
}

fn pixel_at(frame: &Frame, offset_x: u32, offset_y: u32, x: u32, y: u32) -> Rgb8 {
    let Some(local_x) = x.checked_sub(offset_x) else {
        return Rgb8::BLACK;
    };
    let Some(local_y) = y.checked_sub(offset_y) else {
        return Rgb8::BLACK;
    };
    frame.pixel(local_x, local_y).unwrap_or(Rgb8::BLACK)
}

fn set_foreground(output: &mut String, current: &mut Option<Rgb8>, color: Rgb8) {
    if *current == Some(color) {
        return;
    }
    output.push_str(&format!("\x1b[38;2;{};{};{}m", color.r, color.g, color.b));
    *current = Some(color);
}

fn set_background(output: &mut String, current: &mut Option<Rgb8>, color: Rgb8) {
    if *current == Some(color) {
        return;
    }
    output.push_str(&format!("\x1b[48;2;{};{};{}m", color.r, color.g, color.b));
    *current = Some(color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_upper_half_block_with_top_foreground_and_bottom_background() {
        let frame = Frame::new(1, 2, vec![Rgb8::new(255, 0, 0), Rgb8::new(0, 0, 255)]).unwrap();
        let geometry = TerminalGeometry::cells(1, 1).unwrap();
        let output = render_halfblock(&frame, geometry).unwrap();
        let text = String::from_utf8(output.bytes).unwrap();
        assert_eq!(text, "\x1b[H\x1b[38;2;255;0;0m\x1b[48;2;0;0;255m▀\x1b[0m");
    }

    #[test]
    fn full_repaint_overwrites_every_terminal_cell() {
        let frame = Frame::solid(8, 4, Rgb8::new(20, 30, 40)).unwrap();
        let geometry = TerminalGeometry::cells(4, 3).unwrap();
        let output = render_halfblock(&frame, geometry).unwrap();
        let text = String::from_utf8(output.bytes).unwrap();
        let body = text.trim_start_matches("\x1b[H");
        assert_eq!(body.matches('▀').count(), 12);
        assert_eq!(body.matches("\r\n").count(), 2);
    }
}
