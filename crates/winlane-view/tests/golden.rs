use winlane_view::{
    DetectionSource, Frame, GraphicsProtocol, Rgb8, TerminalCapabilities, TerminalEnvironment,
    TerminalGeometry, detect_capabilities, render_frame, render_frame_with_fallback,
};

fn environment_fixture(name: &str) -> TerminalEnvironment {
    let text = match name {
        "kitty.env" => include_str!("../../../tests/fixtures/terminal/kitty.env"),
        "tmux.env" => include_str!("../../../tests/fixtures/terminal/tmux.env"),
        "iterm2.env" => include_str!("../../../tests/fixtures/terminal/iterm2.env"),
        "sixel.env" => include_str!("../../../tests/fixtures/terminal/sixel.env"),
        _ => panic!("unknown fixture {name}"),
    };
    let mut environment = TerminalEnvironment::default();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            panic!("bad fixture line {line}");
        };
        match key {
            "TERM" => environment.term = Some(value.to_owned()),
            "TERM_PROGRAM" => environment.term_program = Some(value.to_owned()),
            "COLORTERM" => environment.colorterm = Some(value.to_owned()),
            "KITTY_WINDOW_ID" => environment.kitty_window_id = Some(value.to_owned()),
            "TMUX" => environment.tmux = Some(value.to_owned()),
            _ => panic!("unknown fixture key {key}"),
        }
    }
    environment
}

fn detection_summary(capabilities: &TerminalCapabilities) -> String {
    format!(
        "protocol={}\nsource={}\ntruecolor={}\ntmux={}\n",
        capabilities.protocol.as_str(),
        capabilities.source.as_str(),
        capabilities.truecolor,
        capabilities.tmux
    )
}

fn assert_detection_fixture(fixture: &str, golden: &[u8]) {
    let capabilities = detect_capabilities(&environment_fixture(fixture));
    assert_eq!(detection_summary(&capabilities).as_bytes(), golden);
}

#[test]
fn kitty_detection_matches_golden() {
    assert_detection_fixture(
        "kitty.env",
        include_bytes!("../../../tests/fixtures/terminal/golden/kitty-detection.golden"),
    );
}

#[test]
fn tmux_detection_conservatively_falls_back_to_blocks() {
    assert_detection_fixture(
        "tmux.env",
        include_bytes!("../../../tests/fixtures/terminal/golden/tmux-detection.golden"),
    );
}

#[test]
fn iterm2_is_detected_but_not_a_supported_renderer() {
    assert_detection_fixture(
        "iterm2.env",
        include_bytes!("../../../tests/fixtures/terminal/golden/iterm2-detection.golden"),
    );
    let frame = Frame::solid(1, 1, Rgb8::BLACK).unwrap();
    let geometry = TerminalGeometry::new(1, 1, 1, 1).unwrap();
    assert!(render_frame(&frame, geometry, GraphicsProtocol::ITerm2).is_err());
    let fallback = render_frame_with_fallback(&frame, geometry, GraphicsProtocol::ITerm2).unwrap();
    assert_eq!(fallback.protocol, GraphicsProtocol::HalfBlock);
}

#[test]
fn sixel_is_detected_but_not_a_supported_renderer() {
    assert_detection_fixture(
        "sixel.env",
        include_bytes!("../../../tests/fixtures/terminal/golden/sixel-detection.golden"),
    );
    assert!(!GraphicsProtocol::Sixel.support().is_supported());
}

#[test]
fn halfblock_escape_output_matches_golden() {
    let frame = Frame::new(
        2,
        2,
        vec![
            Rgb8::new(255, 0, 0),
            Rgb8::new(0, 255, 0),
            Rgb8::new(0, 0, 255),
            Rgb8::new(255, 255, 255),
        ],
    )
    .unwrap();
    let output = render_frame(
        &frame,
        TerminalGeometry::cells(2, 1).unwrap(),
        GraphicsProtocol::HalfBlock,
    )
    .unwrap();
    assert_eq!(
        output.bytes,
        include_bytes!("../../../tests/fixtures/terminal/golden/halfblock-2x2.golden")
    );
}

#[test]
fn kitty_escape_output_matches_golden() {
    let frame = Frame::new(1, 1, vec![Rgb8::new(255, 0, 0)]).unwrap();
    let output = render_frame(
        &frame,
        TerminalGeometry::new(1, 1, 1, 1).unwrap(),
        GraphicsProtocol::Kitty,
    )
    .unwrap();
    assert_eq!(
        output.bytes,
        include_bytes!("../../../tests/fixtures/terminal/golden/kitty-1x1.golden")
    );
}

#[test]
fn detection_sources_are_stable_contract_names() {
    assert_eq!(DetectionSource::KittyWindowId.as_str(), "KITTY_WINDOW_ID");
    assert_eq!(
        DetectionSource::TmuxConservativeFallback.as_str(),
        "tmux conservative fallback"
    );
}
