use std::error::Error;
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use winlane_contracts::FrameRef;
use winlane_view::{
    Frame, GraphicsProtocol, Rgb8, TerminalGeometry, frame_from_ppm, render_frame,
    validate_frame_artifact_hash,
};

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const TARGET_WIDTH: u32 = 640;
const TARGET_HEIGHT: u32 = 480;
const ITERATIONS: usize = 24;

fn frame() -> Result<Frame, Box<dyn Error>> {
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let texture = ((x / 7 + y / 11) % 23) as u8;
            pixels.push(Rgb8::new(
                ((x / 4 + y / 8 + u32::from(texture)) % 251) as u8,
                ((x / 5 + y / 3 + 80) % 241) as u8,
                ((x / 9 + y / 6 + 160) % 229) as u8,
            ));
        }
    }
    Ok(Frame::new(WIDTH, HEIGHT, pixels)?)
}

fn ppm_bytes(frame: &Frame) -> Vec<u8> {
    let mut bytes = format!("P6\n{} {}\n255\n", frame.width(), frame.height()).into_bytes();
    bytes.extend_from_slice(&frame.rgb_bytes());
    bytes
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn timed<T>(
    iterations: usize,
    mut operation: impl FnMut() -> Result<T, Box<dyn Error>>,
) -> Result<(f64, f64), Box<dyn Error>> {
    let mut durations = Vec::with_capacity(iterations);
    operation()?;
    for _ in 0..iterations {
        let started = Instant::now();
        operation()?;
        durations.push(started.elapsed());
    }
    let total: Duration = durations.iter().sum();
    let minimum = durations.iter().min().copied().unwrap_or_default();
    Ok((
        total.as_secs_f64() * 1000.0 / iterations as f64,
        minimum.as_secs_f64() * 1000.0,
    ))
}

fn encode_png(
    rgb: &[u8],
    width: u32,
    height: u32,
    compression: png::Compression,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(compression);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgb)?;
    }
    Ok(encoded)
}

fn encode_png_level1(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_deflate_compression(png::DeflateCompression::Level(1));
        encoder.set_filter(png::Filter::Adaptive);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgb)?;
    }
    Ok(encoded)
}

fn main() -> Result<(), Box<dyn Error>> {
    let frame = frame()?;
    let ppm = ppm_bytes(&frame);
    let metadata = FrameRef {
        sha256: sha256_hex(&ppm),
        width: WIDTH,
        height: HEIGHT,
        captured_unix_ms: 0,
        blob_path: "benchmark.ppm".to_string(),
    };
    let geometry = TerminalGeometry::new(80, 24, 8, 20)?;
    let scaled_frame = frame.scaled_nearest(TARGET_WIDTH, TARGET_HEIGHT)?;
    let scaled_rgb = scaled_frame.rgb_bytes();

    let (decode_avg, decode_min) = timed(ITERATIONS, || Ok(frame_from_ppm(&ppm)?))?;
    let (hash_avg, hash_min) = timed(ITERATIONS, || {
        validate_frame_artifact_hash(&metadata, &ppm)?;
        Ok(())
    })?;
    let (rgb_avg, rgb_min) = timed(ITERATIONS, || Ok(frame.rgb_bytes()))?;
    let (scale_avg, scale_min) = timed(ITERATIONS, || {
        Ok(frame.scaled_catmull_rom_rgb_bytes(TARGET_WIDTH, TARGET_HEIGHT)?)
    })?;
    let (render_scaled_avg, render_scaled_min) = timed(ITERATIONS, || {
        Ok(render_frame(&frame, geometry, GraphicsProtocol::Kitty)?)
    })?;
    let (render_exact_avg, render_exact_min) = timed(ITERATIONS, || {
        Ok(render_frame(
            &scaled_frame,
            geometry,
            GraphicsProtocol::Kitty,
        )?)
    })?;
    let (png_fast_avg, png_fast_min) = timed(ITERATIONS, || {
        encode_png(
            &scaled_rgb,
            TARGET_WIDTH,
            TARGET_HEIGHT,
            png::Compression::Fast,
        )
    })?;
    let (png_fastest_avg, png_fastest_min) = timed(ITERATIONS, || {
        encode_png(
            &scaled_rgb,
            TARGET_WIDTH,
            TARGET_HEIGHT,
            png::Compression::Fastest,
        )
    })?;
    let (png_level1_avg, png_level1_min) = timed(ITERATIONS, || {
        encode_png_level1(&scaled_rgb, TARGET_WIDTH, TARGET_HEIGHT)
    })?;
    let (png_balanced_avg, png_balanced_min) = timed(ITERATIONS, || {
        encode_png(
            &scaled_rgb,
            TARGET_WIDTH,
            TARGET_HEIGHT,
            png::Compression::Balanced,
        )
    })?;
    let fast_png = encode_png(
        &scaled_rgb,
        TARGET_WIDTH,
        TARGET_HEIGHT,
        png::Compression::Fast,
    )?;
    let fastest_png = encode_png(
        &scaled_rgb,
        TARGET_WIDTH,
        TARGET_HEIGHT,
        png::Compression::Fastest,
    )?;
    let level1_png = encode_png_level1(&scaled_rgb, TARGET_WIDTH, TARGET_HEIGHT)?;
    let balanced_png = encode_png(
        &scaled_rgb,
        TARGET_WIDTH,
        TARGET_HEIGHT,
        png::Compression::Balanced,
    )?;
    let scaled_output = render_frame(&frame, geometry, GraphicsProtocol::Kitty)?;

    let result = json!({
        "frame": {"width": WIDTH, "height": HEIGHT, "ppm_bytes": ppm.len()},
        "terminal": {"cols": 80, "rows": 24, "cell_width_px": 8, "cell_height_px": 20},
        "target": {"width": TARGET_WIDTH, "height": TARGET_HEIGHT},
        "iterations": ITERATIONS,
        "average_ms": {
            "ppm_decode": decode_avg,
            "sha256_validation": hash_avg,
            "rgb_conversion": rgb_avg,
            "catmull_rom_scale": scale_avg,
            "kitty_render_with_scale": render_scaled_avg,
            "kitty_render_exact_size": render_exact_avg,
            "png_encode_fast": png_fast_avg,
            "png_encode_fastest": png_fastest_avg,
            "png_encode_balanced": png_balanced_avg,
            "png_encode_level1": png_level1_avg
        },
        "minimum_ms": {
            "ppm_decode": decode_min,
            "sha256_validation": hash_min,
            "rgb_conversion": rgb_min,
            "catmull_rom_scale": scale_min,
            "kitty_render_with_scale": render_scaled_min,
            "kitty_render_exact_size": render_exact_min,
            "png_encode_fast": png_fast_min,
            "png_encode_fastest": png_fastest_min,
            "png_encode_balanced": png_balanced_min,
            "png_encode_level1": png_level1_min
        },
        "bytes": {
            "scaled_frame_rgb": scaled_rgb.len(),
            "png_fast": fast_png.len(),
            "png_fastest": fastest_png.len(),
            "png_balanced": balanced_png.len(),
            "png_level1": level1_png.len(),
            "kitty_output": scaled_output.bytes.len()
        }
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
