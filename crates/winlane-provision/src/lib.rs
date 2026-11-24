//! Windows installation media, answer files, driver integration, golden-image sealing.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WinlaneProvisionError {
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

pub fn implementation_status() -> &'static str {
    "scaffold-only"
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_scaffold_status() {
        assert_eq!(super::implementation_status(), "scaffold-only");
    }
}
