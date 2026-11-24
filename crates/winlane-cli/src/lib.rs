//! User-facing command line interface and integration glue.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WinlaneCliError {
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
