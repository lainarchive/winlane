//! QEMU machine lifecycle, QMP control, frames, input, and checkpoints.

mod error;
mod qemu;
mod qmp;

pub use error::{CoreError, CoreResult};
pub use qemu::{FrameDimensions, QemuBackend};
pub use qmp::{QmpClient, QmpError, QmpEvent};

pub fn implementation_status() -> &'static str {
    "qemu-control-plane"
}
