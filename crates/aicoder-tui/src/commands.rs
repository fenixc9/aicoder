use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommand {
    Exit,
    Resources,
    Reload,
    Permissions,
    Revoke,
    Compact,
    Tree,
    Search,
    SearchNext,
    Rename,
    Label,
    Unlabel,
    Checkout,
    Branch,
    Recover,
    Model,
    Thinking,
    File,
    Image,
    Unimage,
    Detach,
    Theme,
    Usage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentPolicy {
    None,
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub command: SlashCommand,
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub arguments: ArgumentPolicy,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        command: SlashCommand::Exit,
        name: "exit",
        usage: "/exit",
        description: "Exit the TUI",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Resources,
        name: "resources",
        usage: "/resources",
        description: "Show active resource sources",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Reload,
        name: "reload",
        usage: "/reload",
        description: "Reload resources for the next run",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Permissions,
        name: "permissions",
        usage: "/permissions",
        description: "Show current session tool grants",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Revoke,
        name: "revoke",
        usage: "/revoke",
        description: "Revoke current session tool grants",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Compact,
        name: "compact",
        usage: "/compact",
        description: "Summarize current session context",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Tree,
        name: "tree",
        usage: "/tree [CURSOR_JSON]",
        description: "Page the complete branch tree",
        arguments: ArgumentPolicy::Optional,
    },
    CommandSpec {
        command: SlashCommand::Search,
        name: "search",
        usage: "/search QUERY",
        description: "Search recorded visible text",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::SearchNext,
        name: "search-next",
        usage: "/search-next CURSOR_JSON",
        description: "Continue a bounded search",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Rename,
        name: "name",
        usage: "/name TITLE",
        description: "Rename this session",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Label,
        name: "label",
        usage: "/label NAME",
        description: "Label the selected entry",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Unlabel,
        name: "unlabel",
        usage: "/unlabel ENTRY_OR_LABEL",
        description: "Remove an entry label",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Checkout,
        name: "checkout",
        usage: "/checkout ENTRY_OR_LABEL_OR_ROOT",
        description: "Select a branch point",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Branch,
        name: "branch",
        usage: "/branch [ENTRY_OR_LABEL_OR_ROOT]",
        description: "Fork without losing the old branch",
        arguments: ArgumentPolicy::Optional,
    },
    CommandSpec {
        command: SlashCommand::Recover,
        name: "recover",
        usage: "/recover",
        description: "Close a vanished turn without rerunning tools",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Model,
        name: "model",
        usage: "/model MODEL_ID",
        description: "Validate and select a model",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Thinking,
        name: "thinking",
        usage: "/thinking [LEVEL|default]",
        description: "Inspect or validate a reasoning choice",
        arguments: ArgumentPolicy::Optional,
    },
    CommandSpec {
        command: SlashCommand::File,
        name: "file",
        usage: "/file PATH",
        description: "Capture a bounded workspace text page",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Detach,
        name: "detach",
        usage: "/detach [INDEX|all]",
        description: "Remove pending file references",
        arguments: ArgumentPolicy::Optional,
    },
    CommandSpec {
        command: SlashCommand::Theme,
        name: "theme",
        usage: "/theme NAME",
        description: "Select dark, light or mono colors",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Usage,
        name: "usage",
        usage: "/usage",
        description: "Inspect context counts and base-rate estimate",
        arguments: ArgumentPolicy::None,
    },
    CommandSpec {
        command: SlashCommand::Image,
        name: "image",
        usage: "/image PATH",
        description: "Capture a bounded normalized static image",
        arguments: ArgumentPolicy::Required,
    },
    CommandSpec {
        command: SlashCommand::Unimage,
        name: "unimage",
        usage: "/unimage [INDEX|all]",
        description: "Remove pending images",
        arguments: ArgumentPolicy::Optional,
    },
];

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
    if spec.arguments == ArgumentPolicy::Required && arguments(input).is_empty() {
        return Some(Err(SlashCommandError {
            message: format!("Usage: {}", spec.usage),
        }));
    }
    Some(Ok(spec.command))
}

pub fn registered(input: &str) -> bool {
    input
        .trim()
        .strip_prefix('/')
        .and_then(|value| value.split_whitespace().next())
        .is_some_and(|name| all().iter().any(|spec| spec.name == name))
}

pub fn arguments(input: &str) -> &str {
    input
        .trim()
        .split_once(char::is_whitespace)
        .map_or("", |(_, arguments)| arguments.trim())
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
        assert_eq!(all().len(), 23);
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
            vec![
                "exit",
                "resources",
                "reload",
                "permissions",
                "revoke",
                "compact",
                "tree",
                "search",
                "search-next",
                "name",
                "label",
                "unlabel",
                "checkout",
                "branch",
                "recover",
                "model",
                "thinking",
                "file",
                "detach",
                "theme",
                "usage",
                "image",
                "unimage"
            ]
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

#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    fn navigation_arguments_are_validated_without_consuming_titles_or_literal_escape() {
        assert!(parse("/model").unwrap().is_err());
        assert!(registered("/model"));
        assert!(!registered("//model"));
        assert_eq!(parse("/name project title"), Some(Ok(SlashCommand::Rename)));
        assert_eq!(arguments(" /name project title "), "project title");
        assert_eq!(parse("/branch"), Some(Ok(SlashCommand::Branch)));
        assert!(parse("/recover extra").unwrap().is_err());
        assert_eq!(
            parse("/search-next {\"entry\":1}"),
            Some(Ok(SlashCommand::SearchNext))
        );
    }
}
