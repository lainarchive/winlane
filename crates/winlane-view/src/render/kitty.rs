use crate::detect::GraphicsProtocol;
use crate::error::ViewResult;
use crate::frame::Frame;
use crate::geometry::{OutputSurface, TerminalGeometry, plan_frame};
use crate::render::RenderedOutput;

const KITTY_IMAGE_ID: u32 = 1;
const CHUNK_SIZE: usize = 4096;

pub fn render_kitty(frame: &Frame, geometry: TerminalGeometry) -> ViewResult<RenderedOutput> {
    let plan = plan_frame(frame, geometry, OutputSurface::KittyPixels)?;
    let scaled = frame.scaled_nearest(plan.frame_width, plan.frame_height)?;
    let mut output = String::new();
    output.push_str("\x1b_Ga=d,d=I,i=1,q=2\x1b\\");

    let col = plan.offset_x / u32::from(geometry.cell_width_px) + 1;
    let row = plan.offset_y / u32::from(geometry.cell_height_px) + 1;
    output.push_str(&format!("\x1b[{row};{col}H"));
    output.push_str(&encode_kitty_rgb(&scaled));

    Ok(RenderedOutput {
        protocol: GraphicsProtocol::Kitty,
        plan,
        bytes: output.into_bytes(),
    })
}

pub fn encode_kitty_rgb(frame: &Frame) -> String {
    let encoded = base64_encode(&frame.rgb_bytes());
    let mut output = String::with_capacity(encoded.len() + 160);
    let metadata = format!(
        "a=T,f=24,i={},q=2,s={},v={}",
        KITTY_IMAGE_ID,
        frame.width(),
        frame.height()
    );

    if encoded.len() <= CHUNK_SIZE {
        output.push_str(&format!("\x1b_G{metadata};{encoded}\x1b\\"));
        return output;
    }

    let mut first = true;
    for chunk in encoded.as_bytes().chunks(CHUNK_SIZE) {
        let chunk = String::from_utf8_lossy(chunk);
        if first {
            output.push_str(&format!("\x1b_G{metadata},m=1;{chunk}\x1b\\"));
            first = false;
        } else {
            output.push_str(&format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }
    }
    output.push_str("\x1b_Gm=0;\x1b\\");
    output
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = u32::from(*chunk.get(1).unwrap_or(&0));
        let third = u32::from(*chunk.get(2).unwrap_or(&0));
        let value = (first << 16) | (second << 8) | third;
        encoded.push(ALPHABET[(value >> 18) as usize & 0x3f] as char);
        encoded.push(ALPHABET[(value >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            encoded.push(ALPHABET[(value >> 6) as usize & 0x3f] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(ALPHABET[value as usize & 0x3f] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Rgb8;

    #[test]
    fn encodes_rgb_frame_as_single_kitty_chunk() {
        let frame = Frame::new(1, 1, vec![Rgb8::new(255, 0, 0)]).unwrap();
        assert_eq!(
            encode_kitty_rgb(&frame),
            "\x1b_Ga=T,f=24,i=1,q=2,s=1,v=1;/wAA\x1b\\"
        );
    }

    #[test]
    fn chunks_large_payloads_with_empty_final_chunk() {
        let frame = Frame::solid(64, 64, Rgb8::new(1, 2, 3)).unwrap();
        let encoded = encode_kitty_rgb(&frame);
        assert!(encoded.contains("m=1;"));
        assert!(encoded.ends_with("\x1b_Gm=0;\x1b\\"));
    }

    #[test]
    fn render_deletes_previous_image_and_positions_cursor() {
        let frame = Frame::solid(16, 16, Rgb8::new(1, 2, 3)).unwrap();
        let geometry = TerminalGeometry::new(10, 10, 8, 16).unwrap();
        let output = render_kitty(&frame, geometry).unwrap();
        let text = String::from_utf8(output.bytes).unwrap();
        assert!(text.starts_with("\x1b_Ga=d,d=I,i=1,q=2\x1b\\"));
        assert!(text.contains("\x1b[3;1H"));
    }
}
