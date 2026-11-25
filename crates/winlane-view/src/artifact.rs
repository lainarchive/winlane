use sha2::{Digest, Sha256};
use winlane_contracts::FrameRef;

use crate::error::{ViewError, ViewResult};
use crate::frame::{Frame, MAX_FRAME_PIXELS, Rgb8};

/// Validates the artifact hash carried in capture metadata.
pub fn validate_frame_artifact_hash(metadata: &FrameRef, bytes: &[u8]) -> ViewResult<()> {
    if sha256_hex(bytes) != metadata.sha256 {
        return Err(ViewError::FrameArtifactHashMismatch);
    }
    Ok(())
}

/// Decodes artifact dimensions and pixels without validating its hash. Callers
/// must complete hash validation before exposing the frame as a trusted capture.
pub fn frame_from_artifact_without_hash_validation(
    metadata: &FrameRef,
    bytes: &[u8],
) -> ViewResult<Frame> {
    let frame = frame_from_ppm(bytes)?;
    if frame.width() != metadata.width || frame.height() != metadata.height {
        return Err(ViewError::FrameArtifactDimensionMismatch {
            expected_width: metadata.width,
            expected_height: metadata.height,
            actual_width: frame.width(),
            actual_height: frame.height(),
        });
    }
    Ok(frame)
}

pub fn frame_from_artifact(metadata: &FrameRef, bytes: &[u8]) -> ViewResult<Frame> {
    validate_frame_artifact_hash(metadata, bytes)?;
    frame_from_artifact_without_hash_validation(metadata, bytes)
}

pub fn frame_from_ppm(bytes: &[u8]) -> ViewResult<Frame> {
    let mut position = 0;
    let magic = next_ppm_token(bytes, &mut position).ok_or_else(|| invalid("missing PPM magic"))?;
    if magic != b"P6" {
        return Err(invalid("artifact is not a binary PPM frame"));
    }
    let width = parse_ppm_u32(bytes, &mut position, "width")?;
    let height = parse_ppm_u32(bytes, &mut position, "height")?;
    let maxval = parse_ppm_u32(bytes, &mut position, "maximum sample value")?;
    if maxval == 0 || maxval > u32::from(u16::MAX) {
        return Err(invalid("PPM maximum sample value is outside 1..=65535"));
    }
    if position >= bytes.len() || !bytes[position].is_ascii_whitespace() {
        return Err(invalid("PPM header is not followed by raster data"));
    }
    position += 1;

    let bytes_per_sample = if maxval > u32::from(u8::MAX) { 2 } else { 1 };
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| invalid("PPM dimensions overflow host memory calculations"))?;
    if pixel_count > MAX_FRAME_PIXELS {
        return Err(ViewError::FrameTooLarge {
            pixels: pixel_count,
            maximum: MAX_FRAME_PIXELS,
        });
    }
    let expected_len = pixel_count
        .checked_mul(3)
        .and_then(|length| length.checked_mul(bytes_per_sample))
        .ok_or_else(|| invalid("PPM raster length overflows host memory calculations"))?;
    let raster = bytes
        .get(position..)
        .ok_or_else(|| invalid("PPM raster position is outside the artifact"))?;
    if raster.len() != expected_len {
        return Err(invalid("PPM raster length does not match its dimensions"));
    }

    let mut pixels = Vec::with_capacity(pixel_count);
    if maxval == u32::from(u8::MAX) && bytes_per_sample == 1 {
        pixels.extend(
            raster
                .as_chunks::<3>()
                .0
                .iter()
                .map(|sample| Rgb8::new(sample[0], sample[1], sample[2])),
        );
    } else {
        for sample in raster.chunks_exact(3 * bytes_per_sample) {
            let mut channels = [0_u8; 3];
            for (index, channel) in channels.iter_mut().enumerate() {
                let offset = index * bytes_per_sample;
                let value = if bytes_per_sample == 1 {
                    u32::from(sample[offset])
                } else {
                    u32::from(u16::from_be_bytes([sample[offset], sample[offset + 1]]))
                };
                *channel = scale_sample(value, maxval);
            }
            pixels.push(Rgb8::new(channels[0], channels[1], channels[2]));
        }
    }
    Frame::new(width, height, pixels)
}

fn scale_sample(value: u32, maximum: u32) -> u8 {
    ((value * 255 + maximum / 2) / maximum) as u8
}

fn parse_ppm_u32(bytes: &[u8], position: &mut usize, field: &'static str) -> ViewResult<u32> {
    let token = next_ppm_token(bytes, position)
        .ok_or_else(|| invalid("PPM header ended before raster data"))?;
    let text = std::str::from_utf8(&token).map_err(|_| invalid("PPM header is not ASCII"))?;
    let value = text
        .parse::<u32>()
        .map_err(|_| invalid("PPM header field is not an unsigned integer"))?;
    if value == 0 && field != "maximum sample value" {
        return Err(invalid("PPM dimensions must be nonzero"));
    }
    Ok(value)
}

fn next_ppm_token(bytes: &[u8], position: &mut usize) -> Option<Vec<u8>> {
    loop {
        while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
            *position += 1;
        }
        if bytes.get(*position) == Some(&b'#') {
            while bytes.get(*position).is_some_and(|byte| *byte != b'\n') {
                *position += 1;
            }
            continue;
        }
        break;
    }

    let start = *position;
    while bytes
        .get(*position)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'#')
    {
        *position += 1;
    }
    if start == *position {
        return None;
    }
    Some(bytes[start..*position].to_vec())
}

fn invalid(message: &'static str) -> ViewError {
    ViewError::InvalidFrameArtifact(message.to_owned())
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_ppm_with_comments() {
        let bytes = b"P6\n# comment\n2 1\n255\n\xff\x00\x00\x00\x00\xff";
        let frame = frame_from_ppm(bytes).unwrap();
        assert_eq!(frame.width(), 2);
        assert_eq!(frame.height(), 1);
        assert_eq!(frame.pixel(0, 0), Some(Rgb8::new(255, 0, 0)));
        assert_eq!(frame.pixel(1, 0), Some(Rgb8::new(0, 0, 255)));
    }

    #[test]
    fn scales_sixteen_bit_samples() {
        let bytes = b"P6 1 1 65535\n\xff\xff\x00\x00\x80\x00";
        let frame = frame_from_ppm(bytes).unwrap();
        assert_eq!(frame.pixel(0, 0), Some(Rgb8::new(255, 0, 128)));
    }

    #[test]
    fn rejects_truncated_raster() {
        let result = frame_from_ppm(b"P6 2 1 255\n\xff\x00\x00");
        assert!(matches!(result, Err(ViewError::InvalidFrameArtifact(_))));
    }

    #[test]
    fn artifact_hash_validation_accepts_matching_hash_and_rejects_mismatch() {
        let bytes = b"P6 1 1 255
";
        let metadata = FrameRef {
            sha256: sha256_hex(bytes),
            width: 1,
            height: 1,
            captured_unix_ms: 1,
            blob_path: "frame.ppm".to_string(),
        };
        assert!(validate_frame_artifact_hash(&metadata, bytes).is_ok());
        let mismatched = FrameRef {
            sha256: "0".repeat(64),
            ..metadata
        };
        assert!(matches!(
            validate_frame_artifact_hash(&mismatched, bytes),
            Err(ViewError::FrameArtifactHashMismatch)
        ));
    }

    #[test]
    fn unvalidated_decode_still_checks_dimensions() {
        let bytes = b"P6 1 1 255
";
        let metadata = FrameRef {
            sha256: "0".repeat(64),
            width: 2,
            height: 1,
            captured_unix_ms: 1,
            blob_path: "frame.ppm".to_string(),
        };
        assert!(matches!(
            frame_from_artifact_without_hash_validation(&metadata, bytes),
            Err(ViewError::FrameArtifactDimensionMismatch { .. })
        ));
    }
}
