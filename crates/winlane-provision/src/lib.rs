//! Windows installation media, answer files, driver integration, golden-image provisioning.

mod answer;
mod provision;
mod qemu;

pub use provision::{
    ProbeResult, ProvisionError, ProvisionOptions, ProvisionReport, ProvisionResult,
    ProvisionStage, ReadinessReport, ShutdownReport, StageTiming, provision, write_human_log,
};
pub use qemu::{GuestExecStatus, QemuProtocolError};

pub fn implementation_status() -> &'static str {
    "unattended-windows-provisioning"
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_implementation_status() {
        assert_eq!(
            super::implementation_status(),
            "unattended-windows-provisioning"
        );
    }
}
