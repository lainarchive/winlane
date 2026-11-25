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

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::*;
    use crate::frame::Rgb8;

    fn output() -> RenderedOutput {
        let frame = Frame::solid(1, 1, Rgb8::BLACK).unwrap();
        render_frame(
            &frame,
            TerminalGeometry::cells(1, 1).unwrap(),
            GraphicsProtocol::HalfBlock,
        )
        .unwrap()
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_writes_to_noninteractive_sink() {
        let output = output();
        let mut sink = Vec::new();
        output.write_to(&mut sink).unwrap();
        assert_eq!(sink, output.bytes);
    }

    #[test]
    fn writer_errors_are_returned() {
        assert!(matches!(
            output().write_to(&mut FailingWriter),
            Err(ViewError::Output(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn closed_stream_write_is_returned_as_error() {
        use std::os::unix::net::UnixStream;

        let (mut writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        assert!(matches!(
            output().write_to(&mut writer),
            Err(ViewError::Output(_))
        ));
    }
}
