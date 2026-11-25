pub mod detach;
pub mod paste;

use thiserror::Error;
use winlane_contracts::{ControlEvent, MouseButton, ScrollDirection};

pub use detach::{DetachDecision, DetachGuard};
pub use paste::{PasteError, PastePolicy, sanitize_paste};

use crate::geometry::{RenderPlan, TerminalGeometry, terminal_point_to_frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Function(u8),
}

impl Key {
    fn name(self) -> String {
        match self {
            Self::Char(character) => character.to_string(),
            Self::Enter => "enter".to_owned(),
            Self::Escape => "escape".to_owned(),
            Self::Backspace => "backspace".to_owned(),
            Self::Tab => "tab".to_owned(),
            Self::ArrowUp => "up".to_owned(),
            Self::ArrowDown => "down".to_owned(),
            Self::ArrowLeft => "left".to_owned(),
            Self::ArrowRight => "right".to_owned(),
            Self::Home => "home".to_owned(),
            Self::End => "end".to_owned(),
            Self::PageUp => "pageup".to_owned(),
            Self::PageDown => "pagedown".to_owned(),
            Self::Delete => "delete".to_owned(),
            Self::Function(number) => format!("f{number}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyChord {
    pub fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub fn ctrl(key: Key) -> Self {
        Self {
            key,
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    pub fn contract_chord(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl".to_owned());
        }
        if self.alt {
            parts.push("alt".to_owned());
        }
        if self.shift {
            parts.push("shift".to_owned());
        }
        parts.push(self.key.name());
        parts.join("+")
    }

    pub fn plain_character(&self) -> Option<char> {
        if self.ctrl || self.alt {
            return None;
        }
        match self.key {
            Key::Char(character) => Some(character),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseEventKind {
    Click {
        button: MouseButton,
    },
    Move,
    Scroll {
        direction: ScrollDirection,
        steps: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerInput {
    Key(KeyChord),
    Mouse {
        col: u16,
        row: u16,
        kind: MouseEventKind,
    },
    Paste(String),
    Resize {
        cols: u16,
        rows: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputMappingContext {
    pub plan: RenderPlan,
    pub source_width: u32,
    pub source_height: u32,
    pub geometry: TerminalGeometry,
    pub paste_policy: PastePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMapping {
    Forward(ControlEvent),
    Resize { cols: u16, rows: u16 },
    OutsideFrame,
}

#[derive(Debug, Error)]
pub enum InputError {
    #[error(transparent)]
    Paste(#[from] PasteError),
}

pub fn map_input(
    input: &ViewerInput,
    context: InputMappingContext,
) -> Result<InputMapping, InputError> {
    match input {
        ViewerInput::Key(chord) => {
            if let Some(character) = chord.plain_character() {
                return Ok(InputMapping::Forward(ControlEvent::Type {
                    text: character.to_string(),
                }));
            }
            Ok(InputMapping::Forward(ControlEvent::Key {
                chord: chord.contract_chord(),
            }))
        }
        ViewerInput::Mouse { col, row, kind } => match kind {
            MouseEventKind::Scroll { direction, steps } => {
                if terminal_point_to_frame(
                    context.plan,
                    context.source_width,
                    context.source_height,
                    *col,
                    *row,
                    context.geometry,
                )
                .is_none()
                {
                    return Ok(InputMapping::OutsideFrame);
                }
                Ok(InputMapping::Forward(ControlEvent::Scroll {
                    direction: direction.clone(),
                    steps: *steps,
                }))
            }
            MouseEventKind::Click { button } => {
                let Some((x, y)) = terminal_point_to_frame(
                    context.plan,
                    context.source_width,
                    context.source_height,
                    *col,
                    *row,
                    context.geometry,
                ) else {
                    return Ok(InputMapping::OutsideFrame);
                };
                Ok(InputMapping::Forward(ControlEvent::Click {
                    x,
                    y,
                    button: button.clone(),
                }))
            }
            MouseEventKind::Move => {
                let Some((x, y)) = terminal_point_to_frame(
                    context.plan,
                    context.source_width,
                    context.source_height,
                    *col,
                    *row,
                    context.geometry,
                ) else {
                    return Ok(InputMapping::OutsideFrame);
                };
                Ok(InputMapping::Forward(ControlEvent::Move { x, y }))
            }
        },
        ViewerInput::Paste(text) => {
            let sanitized = sanitize_paste(text, context.paste_policy)?;
            Ok(InputMapping::Forward(ControlEvent::Type {
                text: sanitized,
            }))
        }
        ViewerInput::Resize { cols, rows } => Ok(InputMapping::Resize {
            cols: *cols,
            rows: *rows,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::OutputSurface;
    use crate::geometry::plan_render;

    fn context() -> InputMappingContext {
        let geometry = TerminalGeometry::cells(20, 10).unwrap();
        let plan = plan_render(40, 10, geometry, OutputSurface::HalfBlockCells).unwrap();
        InputMappingContext {
            plan,
            source_width: 40,
            source_height: 10,
            geometry,
            paste_policy: PastePolicy::default(),
        }
    }

    #[test]
    fn printable_keys_become_text_input() {
        let mapping = map_input(
            &ViewerInput::Key(KeyChord::plain(Key::Char('a'))),
            context(),
        )
        .unwrap();
        assert_eq!(
            mapping,
            InputMapping::Forward(ControlEvent::Type {
                text: "a".to_owned()
            })
        );
    }

    #[test]
    fn modified_keys_become_contract_chords() {
        let mapping =
            map_input(&ViewerInput::Key(KeyChord::ctrl(Key::Char('c'))), context()).unwrap();
        assert_eq!(
            mapping,
            InputMapping::Forward(ControlEvent::Key {
                chord: "ctrl+c".to_owned()
            })
        );
    }

    #[test]
    fn mouse_click_uses_inverse_scaling_and_letterbox_offsets() {
        let mapping = map_input(
            &ViewerInput::Mouse {
                col: 10,
                row: 4,
                kind: MouseEventKind::Click {
                    button: MouseButton::Left,
                },
            },
            context(),
        )
        .unwrap();
        assert_eq!(
            mapping,
            InputMapping::Forward(ControlEvent::Click {
                x: 20,
                y: 4,
                button: MouseButton::Left,
            })
        );
    }

    #[test]
    fn clicks_in_letterbox_are_not_forwarded() {
        let mapping = map_input(
            &ViewerInput::Mouse {
                col: 10,
                row: 0,
                kind: MouseEventKind::Click {
                    button: MouseButton::Left,
                },
            },
            context(),
        )
        .unwrap();
        assert_eq!(mapping, InputMapping::OutsideFrame);
    }

    #[test]
    fn scroll_inside_frame_is_forwarded() {
        let mapping = map_input(
            &ViewerInput::Mouse {
                col: 10,
                row: 4,
                kind: MouseEventKind::Scroll {
                    direction: ScrollDirection::Down,
                    steps: 2,
                },
            },
            context(),
        )
        .unwrap();
        assert_eq!(
            mapping,
            InputMapping::Forward(ControlEvent::Scroll {
                direction: ScrollDirection::Down,
                steps: 2,
            })
        );
    }

    #[test]
    fn scroll_in_letterbox_is_not_forwarded() {
        let mapping = map_input(
            &ViewerInput::Mouse {
                col: 10,
                row: 0,
                kind: MouseEventKind::Scroll {
                    direction: ScrollDirection::Up,
                    steps: 1,
                },
            },
            context(),
        )
        .unwrap();
        assert_eq!(mapping, InputMapping::OutsideFrame);
    }

    #[test]
    fn paste_is_sanitized_before_becoming_text_input() {
        let mapping = map_input(&ViewerInput::Paste("ok\r\nnext".to_owned()), context()).unwrap();
        assert_eq!(
            mapping,
            InputMapping::Forward(ControlEvent::Type {
                text: "ok\nnext".to_owned()
            })
        );
    }
}
