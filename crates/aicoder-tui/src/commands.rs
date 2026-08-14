use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentPolicy {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub command: SlashCommand,
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub arguments: ArgumentPolicy,
}

const COMMANDS: &[CommandSpec] = &[CommandSpec {
    command: SlashCommand::Exit,
    name: "exit",
    usage: "/exit",
    description: "Exit the TUI",
    arguments: ArgumentPolicy::None,
}];

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

pub fn all() -> &'static [CommandSpec] {
    COMMANDS
}

pub fn suggestions(input: &str) -> Vec<&'static CommandSpec> {
    let Some(query) = palette_query(input) else {
        return Vec::new();
    };
    all()
        .iter()
        .filter(|spec| spec.name.starts_with(query))
        .collect()
}

pub fn completion(spec: &CommandSpec) -> String {
    spec.usage.to_owned()
}

pub fn parse(input: &str) -> Option<Result<SlashCommand, SlashCommandError>> {
    let input = input.trim();
    let command_line = input.strip_prefix('/')?;
    let mut parts = command_line.split_whitespace();
    let name = parts.next().unwrap_or_default();
    let spec = match all().iter().find(|spec| spec.name == name) {
        Some(spec) => spec,
        None => {
            return Some(Err(SlashCommandError {
                message: format!("Unknown command: /{name}"),
            }));
        }
    };
    if spec.arguments == ArgumentPolicy::None && parts.next().is_some() {
        return Some(Err(SlashCommandError {
            message: format!("{} does not accept arguments", spec.usage),
        }));
    }
    Some(Ok(spec.command))
}

fn palette_query(input: &str) -> Option<&str> {
    let input = input.trim_start();
    let query = input.strip_prefix('/')?;
    (!query.chars().any(char::is_whitespace)).then_some(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_exposes_metadata_for_every_command() {
        assert_eq!(all().len(), 1);
        assert_eq!(all()[0].usage, "/exit");
        assert!(!all()[0].description.is_empty());
    }

    #[test]
    fn palette_opens_for_slash_and_filters_by_prefix() {
        assert_eq!(
            suggestions("/")
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            vec!["exit"]
        );
        assert_eq!(
            suggestions("/ex")
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            vec!["exit"]
        );
        assert!(suggestions("/missing").is_empty());
        assert!(suggestions("/exit now").is_empty());
        assert!(suggestions("explain /exit").is_empty());
    }

    #[test]
    fn completes_from_registered_usage() {
        assert_eq!(completion(suggestions("/ex")[0]), "/exit");
    }

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
