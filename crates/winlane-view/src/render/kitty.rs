use crate::detect::GraphicsProtocol;
use crate::error::ViewResult;
use crate::frame::Frame;
use crate::geometry::{OutputSurface, TerminalGeometry, plan_frame};
use crate::render::RenderedOutput;

const KITTY_IMAGE_ID: u32 = 1;
const CHUNK_SIZE: usize = 4096;

pub fn render_kitty(frame: &Frame, geometry: TerminalGeometry) -> ViewResult<RenderedOutput> {
    let plan = plan_frame(frame, geometry, OutputSurface::KittyPixels)?;
    let png = encode_png_rgb(frame)?;
    let mut output = String::new();
    output.push_str("\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    output.push_str("\x1b[H");
    output.push_str(&encode_kitty_png(&png, geometry));

    Ok(RenderedOutput {
        protocol: GraphicsProtocol::Kitty,
        plan,
        bytes: output.into_bytes(),
    })
}

pub fn encode_png_rgb(frame: &Frame) -> ViewResult<Vec<u8>> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, frame.width(), frame.height());
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&frame.rgb_bytes())?;
    }
    Ok(encoded)
}

pub fn encode_kitty_png(png: &[u8], geometry: TerminalGeometry) -> String {
    let metadata = format!(
        "a=T,f=100,i={},q=2,c={},r={},C=1",
        KITTY_IMAGE_ID, geometry.cols, geometry.rows
    );
    encode_kitty_payload(png, &metadata)
}

fn encode_kitty_payload(payload: &[u8], metadata: &str) -> String {
    let encoded = base64_encode(payload);
    let mut output = String::with_capacity(encoded.len() + 160);

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

    fn solid_frame(width: u32, height: u32) -> Frame {
        Frame::solid(width, height, Rgb8::new(1, 2, 3)).unwrap()
    }

    #[test]
    fn png_payload_preserves_source_resolution() {
        let frame = solid_frame(64, 32);
        let png = encode_png_rgb(&frame).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let size = reader.output_buffer_size().unwrap();
        let mut pixels = vec![0; size];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (64, 32));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(&pixels[..3], &[1, 2, 3]);
    }

    #[test]
    fn kitty_output_uses_png_cell_scaling_and_cursor_policy() {
        let frame = solid_frame(64, 32);
        let geometry = TerminalGeometry::new(20, 10, 8, 16).unwrap();
        let output = render_kitty(&frame, geometry).unwrap();
        let text = String::from_utf8(output.bytes).unwrap();
        assert!(text.contains("a=T,f=100"));
        assert!(text.contains("c=20,r=10"));
        assert!(text.contains("C=1"));
        assert!(text.contains("iVBORw0KGgo"));
    }

    #[test]
    fn chunks_large_payloads_with_empty_final_chunk() {
        let frame = solid_frame(512, 512);
        let png = encode_png_rgb(&frame).unwrap();
        let encoded = encode_kitty_png(&png, TerminalGeometry::cells(20, 10).unwrap());
        assert!(encoded.contains("m=1;"));
        assert!(encoded.ends_with("\x1b_Gm=0;\x1b\\"));
    }

    #[test]
    fn render_deletes_previous_image_and_positions_cursor_home() {
        let frame = solid_frame(16, 16);
        let geometry = TerminalGeometry::new(10, 10, 8, 16).unwrap();
        let output = render_kitty(&frame, geometry).unwrap();
        let text = String::from_utf8(output.bytes).unwrap();
        assert!(text.starts_with("\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b[H"));
    }
}
