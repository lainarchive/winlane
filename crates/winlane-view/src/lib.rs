//! Terminal protocol detection, framebuffer rendering, resize planning, and safe input mapping.

pub mod detect;
pub mod error;
pub mod frame;
pub mod geometry;
pub mod harness;
pub mod input;
pub mod render;

pub use detect::{
    DetectionSource, GraphicsProtocol, ProtocolSupport, TerminalCapabilities, TerminalEnvironment,
    detect_capabilities,
};
pub use error::{ViewError, ViewResult};
pub use frame::{Frame, Rgb8};
pub use geometry::{
    OutputSurface, RenderPlan, Resize, TerminalGeometry, ViewportState, plan_frame, plan_render,
    terminal_point_to_frame,
};
pub use input::{
    DetachDecision, DetachGuard, InputError, InputMapping, InputMappingContext, Key, KeyChord,
    MouseEventKind, PasteError, PastePolicy, ViewerInput, map_input, sanitize_paste,
};
pub use render::{RenderedOutput, render_frame, render_frame_with_fallback, write_frame};

pub fn implementation_status() -> &'static str {
    "terminal-core"
}
