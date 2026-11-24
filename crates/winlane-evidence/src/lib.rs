//! Append-only evidence records, storage layout, validation and export.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WinlaneEvidenceError {
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
