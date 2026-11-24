use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent as CrosstermKeyEvent, KeyEventKind, KeyModifiers,
    MouseButton as CrosstermMouseButton, MouseEvent as CrosstermMouseEvent,
    MouseEventKind as CrosstermMouseEventKind,
};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use winlane_contracts::{ControlEvent, MouseButton};
use winlane_contracts::{FrameRef, MachineHandle};
use winlane_ipc::{ControlRequest, UnixControlClient};
use winlane_view::{
    DetachGuard, Frame, InputError, InputMapping, InputMappingContext, Key, KeyChord,
    MouseEventKind as ViewMouseEventKind, PastePolicy, TerminalEnvironment, TerminalGeometry,
    ViewerInput, ViewportState, detect_capabilities, frame_from_artifact, map_input,
    render_frame_with_fallback,
};

use crate::{CliResult, ViewArgs};

const INPUT_POLL_QUANTUM: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct LiveViewConfig {
    pub control_socket: PathBuf,
    pub machine: MachineHandle,
    pub frame_interval: Duration,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

impl LiveViewConfig {
    pub fn from_args(args: ViewArgs) -> CliResult<Self> {
        let machine: MachineHandle = serde_json::from_str(&args.machine_json)?;
        Ok(Self {
            control_socket: args.control_socket,
            machine,
            frame_interval: Duration::from_secs(1) / args.fps,
            cell_width_px: args.cell_width_px,
            cell_height_px: args.cell_height_px,
        })
    }
}

pub async fn capture_frame(
    client: &UnixControlClient,
    machine: &MachineHandle,
) -> CliResult<Frame> {
    let result = client
        .invoke(&ControlRequest::CaptureFrame {
            machine: machine.clone(),
        })
        .await?;
    let metadata: FrameRef = serde_json::from_value(result)?;
    let bytes = tokio::fs::read(&metadata.blob_path).await?;
    Ok(frame_from_artifact(&metadata, &bytes)?)
}

pub async fn run_live_view(config: LiveViewConfig) -> CliResult<()> {
    let capabilities = detect_capabilities(&TerminalEnvironment::capture());
    let protocol = capabilities.protocol;
    let client = UnixControlClient::new(config.control_socket);
    let _terminal = TerminalSession::enter()?;
    let (cols, rows) = terminal::size()?;
    let geometry = TerminalGeometry::new(cols, rows, config.cell_width_px, config.cell_height_px)?;
    let mut session = LiveSession::new(client, config.machine, geometry);
    let mut last_frame = None;
    let mut next_capture = Instant::now();

    loop {
        if Instant::now() >= next_capture {
            let frame = capture_frame(&session.client, &session.machine).await?;
            let output = render_frame_with_fallback(&frame, session.viewport.geometry(), protocol)?;
            output.write_to(&mut session.stdout)?;
            session.viewport.repaint_completed();
            last_frame = Some((output.plan, frame.width(), frame.height()));
            next_capture = Instant::now() + config.frame_interval;
        }

        if session.drain_events(last_frame).await? {
            return Ok(());
        }
        tokio::time::sleep(INPUT_POLL_QUANTUM).await;
    }
}

struct LiveSession {
    client: UnixControlClient,
    machine: MachineHandle,
    viewport: ViewportState,
    detach: DetachGuard,
    stdout: io::Stdout,
}

impl LiveSession {
    fn new(client: UnixControlClient, machine: MachineHandle, geometry: TerminalGeometry) -> Self {
        Self {
            client,
            machine,
            viewport: ViewportState::new(geometry),
            detach: DetachGuard::default(),
            stdout: io::stdout(),
        }
    }

    async fn drain_events(
        &mut self,
        last_frame: Option<(winlane_view::RenderPlan, u32, u32)>,
    ) -> CliResult<bool> {
        while event::poll(Duration::ZERO)? {
            let terminal_event = event::read()?;
            if self.handle_event(terminal_event, last_frame).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn handle_event(
        &mut self,
        terminal_event: Event,
        last_frame: Option<(winlane_view::RenderPlan, u32, u32)>,
    ) -> CliResult<bool> {
        match terminal_event {
            Event::Key(key_event) => {
                let Some(chord) = translate_key_event(key_event) else {
                    return Ok(false);
                };
                match self.detach.observe(&chord) {
                    winlane_view::DetachDecision::Confirmed => return Ok(true),
                    winlane_view::DetachDecision::Armed => return Ok(false),
                    winlane_view::DetachDecision::NotDetach => {}
                }
                if let Some((plan, width, height)) = last_frame {
                    self.forward_input(ViewerInput::Key(chord), plan, width, height)
                        .await?;
                }
            }
            Event::Paste(text) => {
                if let Some((plan, width, height)) = last_frame {
                    self.forward_input(ViewerInput::Paste(text), plan, width, height)
                        .await?;
                }
            }
            Event::Mouse(mouse_event) => {
                if let Some((plan, width, height)) = last_frame
                    && let Some(input) = translate_mouse_event(mouse_event)
                {
                    self.forward_input(input, plan, width, height).await?;
                }
            }
            Event::Resize(cols, rows) => {
                if cols != 0 && rows != 0 {
                    self.viewport.resize(cols, rows)?;
                    execute!(&mut self.stdout, terminal::Clear(ClearType::All))?;
                }
            }
            Event::FocusGained | Event::FocusLost => {}
        }
        Ok(false)
    }

    async fn forward_input(
        &mut self,
        input: ViewerInput,
        plan: winlane_view::RenderPlan,
        source_width: u32,
        source_height: u32,
    ) -> CliResult<()> {
        let context = InputMappingContext {
            plan,
            source_width,
            source_height,
            geometry: self.viewport.geometry(),
            paste_policy: PastePolicy::default(),
        };
        if let Some(event) = control_event_for_input(&input, context) {
            self.client
                .invoke(&ControlRequest::Control {
                    machine: self.machine.clone(),
                    event,
                })
                .await?;
        }
        Ok(())
    }
}

fn control_event_for_input(
    input: &ViewerInput,
    context: InputMappingContext,
) -> Option<ControlEvent> {
    match map_input(input, context) {
        Ok(InputMapping::Forward(event)) => supported_control_event(event),
        Ok(InputMapping::Resize { .. } | InputMapping::OutsideFrame) => None,
        Err(InputError::Paste(_)) => None,
    }
}

fn supported_control_event(event: ControlEvent) -> Option<ControlEvent> {
    if let ControlEvent::Type { text } = &event {
        let supported = text
            .chars()
            .all(|character| matches!(character, ' '..='~' | '\n' | '\t'));
        if !supported {
            return None;
        }
    }
    Some(event)
}

fn translate_key_event(event: CrosstermKeyEvent) -> Option<KeyChord> {
    if matches!(event.kind, KeyEventKind::Release) {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    let key = match event.code {
        KeyCode::Char(character) => {
            if character.is_control() {
                return None;
            }
            Key::Char(character)
        }
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        KeyCode::Up => Key::ArrowUp,
        KeyCode::Down => Key::ArrowDown,
        KeyCode::Left => Key::ArrowLeft,
        KeyCode::Right => Key::ArrowRight,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Delete => Key::Delete,
        KeyCode::F(number) => Key::Function(number),
        _ => return None,
    };
    Some(KeyChord {
        key,
        ctrl,
        alt,
        shift,
    })
}

fn translate_mouse_event(event: CrosstermMouseEvent) -> Option<ViewerInput> {
    let kind = match event.kind {
        CrosstermMouseEventKind::Down(button) => {
            let button = match button {
                CrosstermMouseButton::Left => MouseButton::Left,
                CrosstermMouseButton::Middle => MouseButton::Middle,
                CrosstermMouseButton::Right => MouseButton::Right,
            };
            ViewMouseEventKind::Click { button }
        }
        CrosstermMouseEventKind::Moved => ViewMouseEventKind::Move,
        CrosstermMouseEventKind::ScrollUp => ViewMouseEventKind::Scroll {
            direction: winlane_contracts::ScrollDirection::Up,
            steps: 1,
        },
        CrosstermMouseEventKind::ScrollDown => ViewMouseEventKind::Scroll {
            direction: winlane_contracts::ScrollDirection::Down,
            steps: 1,
        },
        _ => return None,
    };
    Some(ViewerInput::Mouse {
        col: event.column,
        row: event.row,
        kind,
    })
}

struct TerminalSession;

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            Hide,
            EnableMouseCapture,
            EnableBracketedPaste
        ) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            DisableBracketedPaste,
            DisableMouseCapture,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_q_maps_to_detach_binding() {
        let event = CrosstermKeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        let chord = translate_key_event(event).unwrap();
        assert_eq!(&chord, DetachGuard::default().binding());
    }

    #[test]
    fn released_keys_are_ignored() {
        let event = CrosstermKeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(translate_key_event(event), None);
    }

    #[test]
    fn unmodified_control_characters_are_not_forwarded() {
        let event = CrosstermKeyEvent::new_with_kind(
            KeyCode::Char('\x1b'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        assert_eq!(translate_key_event(event), None);
    }

    fn input_context() -> InputMappingContext {
        let geometry = TerminalGeometry::cells(4, 2).unwrap();
        let plan =
            winlane_view::plan_render(4, 4, geometry, winlane_view::OutputSurface::HalfBlockCells)
                .unwrap();
        InputMappingContext {
            plan,
            source_width: 4,
            source_height: 4,
            geometry,
            paste_policy: PastePolicy::default(),
        }
    }

    #[test]
    fn paste_escape_bytes_are_dropped() {
        let input = ViewerInput::Paste("safe\x1b[2J".to_string());
        assert_eq!(control_event_for_input(&input, input_context()), None);
    }

    #[test]
    fn non_ascii_text_is_dropped_for_hardware_keyboard_path() {
        let input = ViewerInput::Paste("echo café".to_string());
        assert_eq!(control_event_for_input(&input, input_context()), None);
    }

    #[test]
    fn modified_control_character_is_ignored() {
        let event = CrosstermKeyEvent::new_with_kind(
            KeyCode::Char('\x11'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(translate_key_event(event), None);
    }

    #[test]
    fn clean_paste_maps_to_guest_text_input() {
        let input = ViewerInput::Paste("echo ok\r\n".to_string());
        assert_eq!(
            control_event_for_input(&input, input_context()),
            Some(ControlEvent::Type {
                text: "echo ok\n".to_string(),
            })
        );
    }

    #[test]
    fn mouse_down_maps_to_guest_click() {
        let event = CrosstermMouseEvent {
            kind: CrosstermMouseEventKind::Down(CrosstermMouseButton::Left),
            column: 3,
            row: 4,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            translate_mouse_event(event),
            Some(ViewerInput::Mouse {
                col: 3,
                row: 4,
                kind: ViewMouseEventKind::Click {
                    button: MouseButton::Left,
                },
            })
        );
    }
}
