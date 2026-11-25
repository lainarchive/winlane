use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PastePolicy {
    pub max_bytes: usize,
    pub allow_newlines: bool,
    pub allow_tabs: bool,
}

impl Default for PastePolicy {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024,
            allow_newlines: true,
            allow_tabs: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PasteError {
    #[error("paste is {actual} bytes; maximum is {maximum}")]
    TooLarge { actual: usize, maximum: usize },
    #[error("paste contains a forbidden control character U+{codepoint:04X}")]
    ControlCharacter { codepoint: u32 },
    #[error("paste contains invisible formatting character U+{codepoint:04X}")]
    InvisibleFormatting { codepoint: u32 },
}

pub fn sanitize_paste(input: &str, policy: PastePolicy) -> Result<String, PasteError> {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.len() > policy.max_bytes {
        return Err(PasteError::TooLarge {
            actual: normalized.len(),
            maximum: policy.max_bytes,
        });
    }

    for character in normalized.chars() {
        if is_invisible_formatting(character) {
            return Err(PasteError::InvisibleFormatting {
                codepoint: character as u32,
            });
        }
        if character.is_control() {
            let allowed = (character == '\n' && policy.allow_newlines)
                || (character == '\t' && policy.allow_tabs);
            if !allowed {
                return Err(PasteError::ControlCharacter {
                    codepoint: character as u32,
                });
            }
        }
    }
    Ok(normalized)
}

fn is_invisible_formatting(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_text_newlines_and_tabs() {
        let paste = sanitize_paste("echo ok\n\texit", PastePolicy::default()).unwrap();
        assert_eq!(paste, "echo ok\n\texit");
    }

    #[test]
    fn normalizes_windows_line_endings() {
        let paste = sanitize_paste("one\r\ntwo\rthree", PastePolicy::default()).unwrap();
        assert_eq!(paste, "one\ntwo\nthree");
    }

    #[test]
    fn rejects_terminal_escape_characters() {
        let result = sanitize_paste("safe\x1b[2J", PastePolicy::default());
        assert!(matches!(
            result,
            Err(PasteError::ControlCharacter { codepoint: 0x1b })
        ));
    }

    #[test]
    fn rejects_invisible_unicode_formatting() {
        let result = sanitize_paste("look\u{202e}edoc", PastePolicy::default());
        assert!(matches!(
            result,
            Err(PasteError::InvisibleFormatting { .. })
        ));
    }

    #[test]
    fn rejects_other_unicode_formatting_characters() {
        for input in ["soft\u{00ad}hyphen", "arabic\u{061c}mark"] {
            let result = sanitize_paste(input, PastePolicy::default());
            assert!(
                matches!(result, Err(PasteError::InvisibleFormatting { .. })),
                "accepted formatting character in {input:?}: {result:?}"
            );
        }
    }

    #[test]
    fn rejects_ascii_controls_except_policy_enabled_newline_and_tab() {
        for codepoint in 0..=0x1f_u32 {
            if matches!(codepoint, 0x09 | 0x0a | 0x0d) {
                continue;
            }
            let input = char::from_u32(codepoint).unwrap().to_string();
            assert!(
                matches!(
                    sanitize_paste(&input, PastePolicy::default()),
                    Err(PasteError::ControlCharacter { .. })
                ),
                "accepted control U+{codepoint:04X}"
            );
        }

        let strict = PastePolicy {
            allow_newlines: false,
            allow_tabs: false,
            ..PastePolicy::default()
        };
        for input in ["\n", "\t"] {
            assert!(matches!(
                sanitize_paste(input, strict),
                Err(PasteError::ControlCharacter { .. })
            ));
        }
    }

    #[test]
    fn enforces_byte_limit_after_normalization() {
        let policy = PastePolicy {
            max_bytes: 3,
            ..PastePolicy::default()
        };
        let result = sanitize_paste("four", policy);
        assert!(matches!(result, Err(PasteError::TooLarge { .. })));
    }
}
