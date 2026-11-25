use std::io::{self, Write};
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
    ViewerInput, ViewportState, ZoomViewport, detect_capabilities, frame_from_artifact, map_input,
    render_frame_with_fallback,
};

use crate::machine::resolve_machine;
use crate::{CliError, CliResult, ViewArgs};

const INPUT_POLL_QUANTUM: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct LiveViewConfig {
    pub control_socket: PathBuf,
    pub machine: MachineHandle,
    pub frame_interval: Duration,
    pub zoom_percent: u16,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

impl LiveViewConfig {
    pub async fn from_args(args: ViewArgs) -> CliResult<Self> {
        let machine =
            resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name).await?;
        Ok(Self {
            control_socket: args.control_socket,
            machine,
            frame_interval: Duration::from_secs(1) / args.fps,
            zoom_percent: args.zoom_percent,
            cell_width_px: args.cell_width_px,
            cell_height_px: args.cell_height_px,
        })
    }
}

#[derive(Debug)]
pub struct CapturedFrame {
    pub frame: Frame,
    pub sha256: String,
}

struct TransientFrameCleanup {
    path: Option<PathBuf>,
}

impl TransientFrameCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    async fn cleanup(mut self) -> io::Result<()> {
        if let Some(path) = self.path.take() {
            tokio::fs::remove_file(path).await?;
        }
        Ok(())
    }
}

impl Drop for TransientFrameCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Captures one transient live-view artifact. The artifact becomes owned by
/// this capture call and is removed after the decode attempt, including
/// failures and cancellation after the path is known.
pub async fn capture_transient_frame(
    client: &UnixControlClient,
    machine: &MachineHandle,
) -> CliResult<CapturedFrame> {
    let result = client
        .invoke(&ControlRequest::CaptureFrame {
            machine: machine.clone(),
        })
        .await?;
    let cleanup = result
        .get("blob_path")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .map(TransientFrameCleanup::new);
    let metadata_result = serde_json::from_value::<FrameRef>(result);
    let mut outcome = match metadata_result {
        Ok(metadata) => match tokio::fs::read(&metadata.blob_path).await {
            Ok(bytes) => frame_from_artifact(&metadata, &bytes)
                .map(|frame| CapturedFrame {
                    frame,
                    sha256: metadata.sha256,
                })
                .map_err(CliError::View),
            Err(error) => Err(error.into()),
        },
        Err(error) => Err(error.into()),
    };
    if let Some(cleanup) = cleanup
        && let Err(error) = cleanup.cleanup().await
        && outcome.is_ok()
    {
        outcome = Err(error.into());
    }
    outcome
}

#[derive(Debug, Clone)]
struct DisplayedFrame {
    plan: winlane_view::RenderPlan,
    frame_width: u32,
    frame_height: u32,
    crop: winlane_view::SourceRect,
    sha256: String,
}

fn terminal_geometry(
    size: &terminal::WindowSize,
    fallback_cell_width: u16,
    fallback_cell_height: u16,
) -> winlane_view::ViewResult<TerminalGeometry> {
    let cell_width = measured_cell_size(size.width, size.columns, fallback_cell_width);
    let cell_height = measured_cell_size(size.height, size.rows, fallback_cell_height);
    TerminalGeometry::new(size.columns, size.rows, cell_width, cell_height)
}

fn measured_cell_size(pixels: u16, cells: u16, fallback: u16) -> u16 {
    if pixels == 0 || cells == 0 {
        return fallback;
    }
    (pixels / cells).max(1)
}

fn fallback_view_rows(rows: u16, fallback: bool) -> u16 {
    if fallback && rows > 1 { rows - 1 } else { rows }
}

fn geometry_from_window_size(
    size: &terminal::WindowSize,
    fallback_cell_width: u16,
    fallback_cell_height: u16,
    fallback: bool,
) -> winlane_view::ViewResult<(TerminalGeometry, u16, u16)> {
    let full_geometry = terminal_geometry(size, fallback_cell_width, fallback_cell_height)?;
    let geometry = TerminalGeometry::new(
        full_geometry.cols,
        fallback_view_rows(full_geometry.rows, fallback),
        full_geometry.cell_width_px,
        full_geometry.cell_height_px,
    )?;
    Ok((geometry, full_geometry.cols, full_geometry.rows))
}

fn read_live_geometry(
    fallback_cell_width: u16,
    fallback_cell_height: u16,
    fallback: bool,
) -> CliResult<(TerminalGeometry, u16, u16)> {
    Ok(geometry_from_window_size(
        &terminal::window_size()?,
        fallback_cell_width,
        fallback_cell_height,
        fallback,
    )?)
}

pub async fn run_live_view(config: LiveViewConfig) -> CliResult<()> {
    let capabilities = detect_capabilities(&TerminalEnvironment::capture());
    let protocol = capabilities.protocol;
    let client = UnixControlClient::new(config.control_socket);
    let _terminal = TerminalSession::enter()?;
    let fallback = protocol != winlane_view::GraphicsProtocol::Kitty;
    let fallback_cell_width = config.cell_width_px;
    let fallback_cell_height = config.cell_height_px;
    let (geometry, terminal_cols, terminal_rows) =
        read_live_geometry(fallback_cell_width, fallback_cell_height, fallback)?;
    let mut session = LiveSession::new(
        client,
        config.machine,
        geometry,
        (terminal_cols, terminal_rows),
        fallback,
        (fallback_cell_width, fallback_cell_height),
        config.zoom_percent,
    )?;
    let mut last_frame: Option<DisplayedFrame> = None;
    let mut next_capture = Instant::now();

    loop {
        if Instant::now() >= next_capture {
            let captured = capture_transient_frame(&session.client, &session.machine).await?;
            let crop = session
                .zoom
                .rect_for(captured.frame.width(), captured.frame.height())?;
            if frame_repaint_required(
                last_frame.as_ref(),
                &captured.sha256,
                crop,
                session.viewport.full_repaint_required(),
            ) {
                let visible_frame = captured.frame.crop(crop)?;
                let output = render_frame_with_fallback(
                    &visible_frame,
                    session.viewport.geometry(),
                    protocol,
                )?;
                output.write_to(&mut session.stdout)?;
                session.write_fallback_label()?;
                session.viewport.repaint_completed();
                last_frame = Some(DisplayedFrame {
                    plan: output.plan,
                    frame_width: captured.frame.width(),
                    frame_height: captured.frame.height(),
                    crop,
                    sha256: captured.sha256,
                });
            }
            next_capture = Instant::now() + config.frame_interval;
        }

        if session.drain_events(last_frame.as_ref()).await? {
            return Ok(());
        }
        tokio::time::sleep(INPUT_POLL_QUANTUM).await;
    }
}

struct LiveSession {
    client: UnixControlClient,
    machine: MachineHandle,
    viewport: ViewportState,
    zoom: ZoomViewport,
    detach: DetachGuard,
    stdout: io::Stdout,
    terminal_cols: u16,
    terminal_rows: u16,
    fallback: bool,
    fallback_cell_width: u16,
    fallback_cell_height: u16,
}

impl LiveSession {
    fn new(
        client: UnixControlClient,
        machine: MachineHandle,
        geometry: TerminalGeometry,
        terminal_size: (u16, u16),
        fallback: bool,
        cell_fallbacks: (u16, u16),
        zoom_percent: u16,
    ) -> winlane_view::ViewResult<Self> {
        Ok(Self {
            client,
            machine,
            viewport: ViewportState::new(geometry),
            zoom: ZoomViewport::new(zoom_percent)?,
            detach: DetachGuard::default(),
            stdout: io::stdout(),
            terminal_cols: terminal_size.0,
            terminal_rows: terminal_size.1,
            fallback,
            fallback_cell_width: cell_fallbacks.0,
            fallback_cell_height: cell_fallbacks.1,
        })
    }

    async fn drain_events(&mut self, last_frame: Option<&DisplayedFrame>) -> CliResult<bool> {
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
        last_frame: Option<&DisplayedFrame>,
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
                if let Some(displayed) = last_frame {
                    if self.pan_viewport(&chord, displayed)? {
                        return Ok(false);
                    }
                    self.forward_input(ViewerInput::Key(chord), displayed)
                        .await?;
                }
            }
            Event::Paste(text) => {
                if let Some(displayed) = last_frame {
                    self.forward_input(ViewerInput::Paste(text), displayed)
                        .await?;
                }
            }
            Event::Mouse(mouse_event) => {
                if let Some(displayed) = last_frame
                    && let Some(input) = translate_mouse_event(mouse_event)
                {
                    self.forward_input(input, displayed).await?;
                }
            }
            Event::Resize(cols, rows) => {
                if cols != 0 && rows != 0 {
                    let (geometry, measured_cols, measured_rows) = read_live_geometry(
                        self.fallback_cell_width,
                        self.fallback_cell_height,
                        self.fallback,
                    )?;
                    self.terminal_cols = measured_cols;
                    self.terminal_rows = measured_rows;
                    self.viewport.resize_geometry(geometry)?;
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
        displayed: &DisplayedFrame,
    ) -> CliResult<()> {
        let context = InputMappingContext {
            plan: displayed.plan,
            source_width: displayed.crop.width,
            source_height: displayed.crop.height,
            geometry: self.viewport.geometry(),
            paste_policy: PastePolicy::default(),
        };
        if let Some(event) = control_event_for_input(&input, context) {
            self.client
                .invoke(&ControlRequest::Control {
                    machine: self.machine.clone(),
                    event: offset_control_event(event, displayed.crop),
                })
                .await?;
        }
        Ok(())
    }

    fn pan_viewport(&mut self, chord: &KeyChord, displayed: &DisplayedFrame) -> CliResult<bool> {
        if self.zoom.zoom_percent() == 100 || !chord.ctrl || chord.alt || chord.shift {
            return Ok(false);
        }
        let horizontal = (displayed.crop.width / 4).max(1) as i32;
        let vertical = (displayed.crop.height / 4).max(1) as i32;
        let (dx, dy) = match chord.key {
            Key::ArrowLeft => (-horizontal, 0),
            Key::ArrowRight => (horizontal, 0),
            Key::ArrowUp => (0, -vertical),
            Key::ArrowDown => (0, vertical),
            _ => return Ok(false),
        };
        self.zoom
            .pan_by(dx, dy, displayed.frame_width, displayed.frame_height)?;
        Ok(true)
    }

    fn write_fallback_label(&mut self) -> io::Result<()> {
        if !self.fallback {
            return Ok(());
        }
        if let Some(sequence) = fallback_label_sequence(self.terminal_rows, self.terminal_cols) {
            write!(self.stdout, "{sequence}")?;
            self.stdout.flush()?;
        }
        Ok(())
    }
}

fn frame_repaint_required(
    last_frame: Option<&DisplayedFrame>,
    sha256: &str,
    crop: winlane_view::SourceRect,
    viewport_repaint_required: bool,
) -> bool {
    if viewport_repaint_required {
        return true;
    }
    match last_frame {
        Some(displayed) => displayed.sha256 != sha256 || displayed.crop != crop,
        None => true,
    }
}

fn fallback_label_sequence(rows: u16, cols: u16) -> Option<String> {
    if rows < 2 || cols < 2 {
        return None;
    }
    let width = usize::from(cols - 1);
    let label: String = " HALF-BLOCK PREVIEW ".chars().take(width).collect();
    Some(format!("\x1b[{rows};1H\x1b[7m{label}\x1b[0m\x1b[K"))
}

fn offset_control_event(event: ControlEvent, crop: winlane_view::SourceRect) -> ControlEvent {
    match event {
        ControlEvent::Click { x, y, button } => ControlEvent::Click {
            x: x.saturating_add(crop.x),
            y: y.saturating_add(crop.y),
            button,
        },
        ControlEvent::Move { x, y } => ControlEvent::Move {
            x: x.saturating_add(crop.x),
            y: y.saturating_add(crop.y),
        },
        other => other,
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

    #[test]
    fn transient_cleanup_drop_removes_owned_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("frame.ppm");
        std::fs::write(&path, b"frame").unwrap();
        drop(TransientFrameCleanup::new(path.clone()));
        assert!(!path.exists());
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
    fn terminal_geometry_uses_measured_cell_pixels_when_available() {
        let size = terminal::WindowSize {
            rows: 20,
            columns: 80,
            width: 800,
            height: 400,
        };
        let geometry = terminal_geometry(&size, 8, 16).unwrap();
        assert_eq!(geometry.cell_width_px, 10);
        assert_eq!(geometry.cell_height_px, 20);
    }

    #[test]
    fn terminal_geometry_uses_configured_cell_pixels_when_ioctl_pixels_are_zero() {
        let size = terminal::WindowSize {
            rows: 20,
            columns: 80,
            width: 0,
            height: 0,
        };
        let geometry = terminal_geometry(&size, 9, 17).unwrap();
        assert_eq!(geometry.cell_width_px, 9);
        assert_eq!(geometry.cell_height_px, 17);
    }

    #[test]
    fn live_geometry_reflects_font_zoom_cell_pixel_changes() {
        let zoomed = terminal::WindowSize {
            rows: 10,
            columns: 40,
            width: 800,
            height: 400,
        };
        let (geometry, cols, rows) = geometry_from_window_size(&zoomed, 8, 16, false).unwrap();
        assert_eq!((cols, rows), (40, 10));
        assert_eq!((geometry.cols, geometry.rows), (40, 10));
        assert_eq!((geometry.cell_width_px, geometry.cell_height_px), (20, 40));
    }

    #[test]
    fn fallback_label_is_obvious_and_bounded_to_last_row() {
        let sequence = fallback_label_sequence(24, 80).unwrap();
        assert_eq!(
            sequence,
            "\x1b[24;1H\x1b[7m HALF-BLOCK PREVIEW \x1b[0m\x1b[K"
        );
        assert!(fallback_label_sequence(1, 80).is_none());
    }

    #[test]
    fn identical_frame_skips_repaint_but_crop_resize_or_new_hash_repaints() {
        let geometry = TerminalGeometry::cells(4, 2).unwrap();
        let plan =
            winlane_view::plan_render(4, 4, geometry, winlane_view::OutputSurface::HalfBlockCells)
                .unwrap();
        let crop = winlane_view::SourceRect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let displayed = DisplayedFrame {
            plan,
            frame_width: 4,
            frame_height: 4,
            crop,
            sha256: "same".to_string(),
        };
        assert!(!frame_repaint_required(
            Some(&displayed),
            "same",
            crop,
            false
        ));
        assert!(frame_repaint_required(Some(&displayed), "new", crop, false));
        assert!(frame_repaint_required(
            Some(&displayed),
            "same",
            winlane_view::SourceRect { x: 1, ..crop },
            false
        ));
        assert!(frame_repaint_required(Some(&displayed), "same", crop, true));
        assert!(frame_repaint_required(None, "same", crop, false));
    }

    #[test]
    fn crop_offsets_mouse_coordinates_back_to_source_frame() {
        let crop = winlane_view::SourceRect {
            x: 100,
            y: 200,
            width: 10,
            height: 10,
        };
        assert_eq!(
            offset_control_event(
                ControlEvent::Click {
                    x: 3,
                    y: 4,
                    button: MouseButton::Left,
                },
                crop,
            ),
            ControlEvent::Click {
                x: 103,
                y: 204,
                button: MouseButton::Left,
            }
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
