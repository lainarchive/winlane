//! Terminal protocol detection, framebuffer rendering, resize planning, and safe input mapping.

pub mod artifact;
pub mod detect;
pub mod error;
pub mod frame;
pub mod geometry;
pub mod harness;
pub mod input;
pub mod render;
pub mod viewport;

pub use artifact::{
    frame_from_artifact, frame_from_artifact_without_hash_validation, frame_from_ppm,
    validate_frame_artifact_hash,
};
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
pub use viewport::{SourceRect, ZoomViewport};

pub fn implementation_status() -> &'static str {
    "terminal-core"
}
