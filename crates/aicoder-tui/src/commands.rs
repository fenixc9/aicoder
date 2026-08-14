use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandError {
    message: String,
}

impl fmt::Display for SlashCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SlashCommandError {}

pub fn parse(input: &str) -> Option<Result<SlashCommand, SlashCommandError>> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }

    let mut parts = input.split_whitespace();
    let name = parts.next().unwrap_or_default();
    let has_arguments = parts.next().is_some();
    let result = match name {
        "/exit" if !has_arguments => Ok(SlashCommand::Exit),
        "/exit" => Err(SlashCommandError {
            message: "/exit does not accept arguments".into(),
        }),
        _ => Err(SlashCommandError {
            message: format!("Unknown command: {name}"),
        }),
    };
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exit_with_surrounding_whitespace() {
        assert_eq!(parse("  /exit  "), Some(Ok(SlashCommand::Exit)));
    }

    #[test]
    fn leaves_normal_prompts_for_the_agent() {
        assert_eq!(parse("explain /exit"), None);
    }

    #[test]
    fn rejects_unknown_commands_and_exit_arguments() {
        assert_eq!(
            parse("/unknown").unwrap().unwrap_err().to_string(),
            "Unknown command: /unknown"
        );
        assert_eq!(
            parse("/exit now").unwrap().unwrap_err().to_string(),
            "/exit does not accept arguments"
        );
    }
}
