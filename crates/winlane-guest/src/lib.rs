//! In-guest helper protocol, UIA extraction, process and file control.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WinlaneGuestError {
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
