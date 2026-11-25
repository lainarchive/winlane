pub mod halfblock;
pub mod kitty;

use std::io::Write;

use crate::detect::GraphicsProtocol;
use crate::error::{ViewError, ViewResult};
use crate::frame::Frame;
use crate::geometry::{RenderPlan, TerminalGeometry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedOutput {
    pub protocol: GraphicsProtocol,
    pub plan: RenderPlan,
    pub bytes: Vec<u8>,
}

impl RenderedOutput {
    pub fn write_to<W: Write>(&self, writer: &mut W) -> ViewResult<()> {
        writer.write_all(&self.bytes)?;
        writer.flush()?;
        Ok(())
    }
}

pub fn render_frame(
    frame: &Frame,
    geometry: TerminalGeometry,
    protocol: GraphicsProtocol,
) -> ViewResult<RenderedOutput> {
    match protocol {
        GraphicsProtocol::HalfBlock => halfblock::render_halfblock(frame, geometry),
        GraphicsProtocol::Kitty => kitty::render_kitty(frame, geometry),
        GraphicsProtocol::ITerm2 => Err(ViewError::UnsupportedProtocol {
            protocol: protocol.as_str(),
        }),
        GraphicsProtocol::Sixel => Err(ViewError::UnsupportedProtocol {
            protocol: protocol.as_str(),
        }),
    }
}

pub fn render_frame_with_fallback(
    frame: &Frame,
    geometry: TerminalGeometry,
    protocol: GraphicsProtocol,
) -> ViewResult<RenderedOutput> {
    if protocol.support().is_supported() {
        render_frame(frame, geometry, protocol)
    } else {
        halfblock::render_halfblock(frame, geometry)
    }
}

pub fn write_frame<W: Write>(
    frame: &Frame,
    geometry: TerminalGeometry,
    protocol: GraphicsProtocol,
    writer: &mut W,
) -> ViewResult<RenderPlan> {
    let output = render_frame(frame, geometry, protocol)?;
    output.write_to(writer)?;
    Ok(output.plan)
}
