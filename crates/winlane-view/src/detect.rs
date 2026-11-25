use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    Kitty,
    ITerm2,
    Sixel,
    HalfBlock,
}

impl GraphicsProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::ITerm2 => "iterm2",
            Self::Sixel => "sixel",
            Self::HalfBlock => "half-block",
        }
    }

    pub fn support(self) -> ProtocolSupport {
        match self {
            Self::Kitty | Self::HalfBlock => ProtocolSupport::Supported,
            Self::ITerm2 => ProtocolSupport::DetectedUnsupported {
                reason: "iTerm2 inline images are detected but not encoded; half-block fallback is required",
            },
            Self::Sixel => ProtocolSupport::DetectedUnsupported {
                reason: "sixel is detected but not encoded; half-block fallback is required",
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolSupport {
    Supported,
    DetectedUnsupported { reason: &'static str },
}

impl ProtocolSupport {
    pub fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionSource {
    KittyWindowId,
    KittyTerm,
    Ghostty,
    WezTerm,
    ITerm2,
    SixelTerm,
    TmuxConservativeFallback,
    TruecolorFallback,
    DefaultFallback,
}

impl DetectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KittyWindowId => "KITTY_WINDOW_ID",
            Self::KittyTerm => "kitty TERM",
            Self::Ghostty => "Ghostty TERM_PROGRAM",
            Self::WezTerm => "WezTerm TERM_PROGRAM",
            Self::ITerm2 => "iTerm2 TERM_PROGRAM",
            Self::SixelTerm => "sixel TERM",
            Self::TmuxConservativeFallback => "tmux conservative fallback",
            Self::TruecolorFallback => "truecolor fallback",
            Self::DefaultFallback => "default fallback",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalEnvironment {
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub colorterm: Option<String>,
    pub kitty_window_id: Option<String>,
    pub tmux: Option<String>,
}

impl TerminalEnvironment {
    pub fn capture() -> Self {
        Self {
            term: env::var("TERM").ok(),
            term_program: env::var("TERM_PROGRAM").ok(),
            colorterm: env::var("COLORTERM").ok(),
            kitty_window_id: env::var("KITTY_WINDOW_ID").ok(),
            tmux: env::var("TMUX").ok(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub protocol: GraphicsProtocol,
    pub source: DetectionSource,
    pub truecolor: bool,
    pub tmux: bool,
}

impl TerminalCapabilities {
    pub fn renderer_is_supported(&self) -> bool {
        self.protocol.support().is_supported()
    }
}

pub fn detect_capabilities(environment: &TerminalEnvironment) -> TerminalCapabilities {
    let truecolor = environment
        .colorterm
        .as_deref()
        .map(|value| {
            let normalized = value.to_ascii_lowercase();
            normalized == "truecolor" || normalized == "24bit"
        })
        .unwrap_or(false);
    let tmux = environment
        .tmux
        .as_deref()
        .is_some_and(|value| !value.is_empty());

    let term = environment
        .term
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let term_program = environment
        .term_program
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if tmux {
        return TerminalCapabilities {
            protocol: GraphicsProtocol::HalfBlock,
            source: DetectionSource::TmuxConservativeFallback,
            truecolor,
            tmux,
        };
    }

    if environment
        .kitty_window_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return capabilities(
            GraphicsProtocol::Kitty,
            DetectionSource::KittyWindowId,
            truecolor,
            tmux,
        );
    }
    if term.contains("kitty") {
        return capabilities(
            GraphicsProtocol::Kitty,
            DetectionSource::KittyTerm,
            truecolor,
            tmux,
        );
    }
    if term_program == "ghostty" {
        return capabilities(
            GraphicsProtocol::Kitty,
            DetectionSource::Ghostty,
            truecolor,
            tmux,
        );
    }
    if term_program == "wezterm" {
        return capabilities(
            GraphicsProtocol::Kitty,
            DetectionSource::WezTerm,
            truecolor,
            tmux,
        );
    }
    if term_program == "iterm.app" {
        return capabilities(
            GraphicsProtocol::ITerm2,
            DetectionSource::ITerm2,
            truecolor,
            tmux,
        );
    }
    if term.contains("sixel") || term_program == "mlterm" {
        return capabilities(
            GraphicsProtocol::Sixel,
            DetectionSource::SixelTerm,
            truecolor,
            tmux,
        );
    }

    let source = if truecolor {
        DetectionSource::TruecolorFallback
    } else {
        DetectionSource::DefaultFallback
    };
    capabilities(GraphicsProtocol::HalfBlock, source, truecolor, tmux)
}

fn capabilities(
    protocol: GraphicsProtocol,
    source: DetectionSource,
    truecolor: bool,
    tmux: bool,
) -> TerminalCapabilities {
    TerminalCapabilities {
        protocol,
        source,
        truecolor,
        tmux,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(fields: &[(&str, &str)]) -> TerminalEnvironment {
        let mut environment = TerminalEnvironment::default();
        for (key, value) in fields {
            match *key {
                "TERM" => environment.term = Some((*value).to_owned()),
                "TERM_PROGRAM" => environment.term_program = Some((*value).to_owned()),
                "COLORTERM" => environment.colorterm = Some((*value).to_owned()),
                "KITTY_WINDOW_ID" => environment.kitty_window_id = Some((*value).to_owned()),
                "TMUX" => environment.tmux = Some((*value).to_owned()),
                _ => {}
            }
        }
        environment
    }

    #[test]
    fn tmux_overrides_native_graphics_detection() {
        let detected = detect_capabilities(&env(&[
            ("TMUX", "/tmp/tmux"),
            ("KITTY_WINDOW_ID", "7"),
            ("COLORTERM", "truecolor"),
        ]));
        assert_eq!(detected.protocol, GraphicsProtocol::HalfBlock);
        assert_eq!(detected.source, DetectionSource::TmuxConservativeFallback);
    }

    #[test]
    fn kitty_terminal_detection_prefers_window_id() {
        let detected =
            detect_capabilities(&env(&[("TERM", "xterm-kitty"), ("KITTY_WINDOW_ID", "7")]));
        assert_eq!(detected.protocol, GraphicsProtocol::Kitty);
        assert_eq!(detected.source, DetectionSource::KittyWindowId);
    }

    #[test]
    fn ghostty_uses_kitty_graphics() {
        let detected = detect_capabilities(&env(&[("TERM_PROGRAM", "ghostty")]));
        assert_eq!(detected.protocol, GraphicsProtocol::Kitty);
        assert_eq!(detected.source, DetectionSource::Ghostty);
    }

    #[test]
    fn iterm_is_detected_but_not_claimed_supported() {
        let detected = detect_capabilities(&env(&[("TERM_PROGRAM", "iTerm.app")]));
        assert_eq!(detected.protocol, GraphicsProtocol::ITerm2);
        assert!(!detected.renderer_is_supported());
    }

    #[test]
    fn terminal_matrix_detection_rows() {
        let rows = [
            (
                "kitty TERM plus window id",
                vec![
                    ("TERM", "xterm-kitty"),
                    ("KITTY_WINDOW_ID", "7"),
                    ("COLORTERM", "truecolor"),
                ],
                GraphicsProtocol::Kitty,
                DetectionSource::KittyWindowId,
                true,
                true,
            ),
            (
                "xterm truecolor",
                vec![("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
                GraphicsProtocol::HalfBlock,
                DetectionSource::TruecolorFallback,
                true,
                true,
            ),
            (
                "xterm default color",
                vec![("TERM", "xterm")],
                GraphicsProtocol::HalfBlock,
                DetectionSource::DefaultFallback,
                false,
                true,
            ),
            (
                "tmux conservative override",
                vec![
                    ("TERM", "xterm-kitty"),
                    ("KITTY_WINDOW_ID", "7"),
                    ("TMUX", "/tmp/tmux-1000/default,1,0"),
                ],
                GraphicsProtocol::HalfBlock,
                DetectionSource::TmuxConservativeFallback,
                false,
                true,
            ),
            (
                "iterm2 detected unsupported",
                vec![("TERM_PROGRAM", "iTerm.app")],
                GraphicsProtocol::ITerm2,
                DetectionSource::ITerm2,
                false,
                false,
            ),
            (
                "sixel TERM detected unsupported",
                vec![("TERM", "xterm-sixel")],
                GraphicsProtocol::Sixel,
                DetectionSource::SixelTerm,
                false,
                false,
            ),
            (
                "mlterm sixel detected unsupported",
                vec![("TERM_PROGRAM", "mlterm")],
                GraphicsProtocol::Sixel,
                DetectionSource::SixelTerm,
                false,
                false,
            ),
        ];

        for (name, fields, protocol, source, truecolor, supported) in rows {
            let detected = detect_capabilities(&env(&fields));
            assert_eq!(detected.protocol, protocol, "{name}");
            assert_eq!(detected.source, source, "{name}");
            assert_eq!(detected.truecolor, truecolor, "{name}");
            assert_eq!(detected.renderer_is_supported(), supported, "{name}");
        }
    }
}
