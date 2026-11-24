use std::path::PathBuf;

use crate::CliError;

pub const VIEW_USAGE: &str = "usage: winlane view --control-socket PATH --machine-json JSON [--fps N] [--cell-width-px N] [--cell-height-px N]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewArgs {
    pub control_socket: PathBuf,
    pub machine_json: String,
    pub fps: u32,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    View(ViewArgs),
}

pub fn parse_args(args: &[String]) -> Result<Command, CliError> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Command::Help);
    }
    if args.first().is_some_and(|arg| arg != "view") {
        return Err(CliError::Usage(format!(
            "unknown command {}; {VIEW_USAGE}",
            args.first().map(String::as_str).unwrap_or_default()
        )));
    }

    let mut control_socket = None;
    let mut machine_json = None;
    let mut fps = 5_u32;
    let mut cell_width_px = 8_u16;
    let mut cell_height_px = 16_u16;
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        index += 1;
        let value = args
            .get(index)
            .ok_or_else(|| CliError::Usage(format!("missing value for {flag}; {VIEW_USAGE}")))?;
        index += 1;
        match flag.as_str() {
            "--control-socket" => control_socket = Some(PathBuf::from(value)),
            "--machine-json" => machine_json = Some(value.clone()),
            "--fps" => fps = parse_bounded(value, "fps", 1, 30)?,
            "--cell-width-px" => {
                cell_width_px = parse_bounded(value, "cell width", 1, 512)?;
            }
            "--cell-height-px" => {
                cell_height_px = parse_bounded(value, "cell height", 1, 512)?;
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown view flag {flag}; {VIEW_USAGE}"
                )));
            }
        }
    }

    let control_socket = control_socket
        .ok_or_else(|| CliError::Usage(format!("missing --control-socket; {VIEW_USAGE}")))?;
    let machine_json = machine_json
        .ok_or_else(|| CliError::Usage(format!("missing --machine-json; {VIEW_USAGE}")))?;
    Ok(Command::View(ViewArgs {
        control_socket,
        machine_json,
        fps,
        cell_width_px,
        cell_height_px,
    }))
}

fn parse_bounded<T>(value: &str, name: &str, minimum: T, maximum: T) -> Result<T, CliError>
where
    T: std::str::FromStr + PartialOrd + Copy,
{
    let parsed = value
        .parse::<T>()
        .map_err(|_| CliError::Usage(format!("{name} must be an integer; {VIEW_USAGE}")))?;
    if parsed < minimum || parsed > maximum {
        return Err(CliError::Usage(format!(
            "{name} is outside the supported range; {VIEW_USAGE}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_view_command() {
        let args = vec![
            "view".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Command::View(ViewArgs {
                control_socket: PathBuf::from("/tmp/control.sock"),
                machine_json: "{}".to_string(),
                fps: 5,
                cell_width_px: 8,
                cell_height_px: 16,
            })
        );
    }

    #[test]
    fn rejects_unknown_command() {
        let result = parse_args(&["machine".to_string()]);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn rejects_missing_flag_value() {
        let result = parse_args(&["view".to_string(), "--fps".to_string()]);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }
}
