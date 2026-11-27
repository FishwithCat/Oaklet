//! Mention commands typed in the composer.
//!
//! The backend reads `@arch` for read-only architecture planning, `@worktree`
//! to create the chat's worktree, and `@queue` to wait for the next turn.
//! The desktop test checks that `packages/desktop/src/features/chat/mentions.ts`
//! and this list use the same command names.

use serde::Serialize;

/// One command the composer can offer and the backend recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MentionCommand {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[MentionCommand] = &[
    MentionCommand {
        name: "arch",
        description: "Plan an architecture change with a LikeC4 model diff",
    },
    MentionCommand {
        name: "worktree",
        description: "Start this chat in a new Git worktree",
    },
    MentionCommand {
        name: "queue",
        description: "Send after the current turn finishes",
    },
];

/// Commands mentioned in `text`, in order, without duplicates.
pub fn parse(text: &str) -> Vec<&'static MentionCommand> {
    let mut found: Vec<&'static MentionCommand> = Vec::new();
    for (start, _) in text.match_indices('@') {
        if !starts_token(text, start) {
            continue;
        }
        let rest = &text[start + 1..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() {
            continue;
        }
        if let Some(command) = COMMANDS
            .iter()
            .find(|command| command.name.eq_ignore_ascii_case(name))
            && !found.iter().any(|found| found.name == command.name)
        {
            found.push(command);
        }
    }
    found
}

pub fn has(text: &str, name: &str) -> bool {
    parse(text).iter().any(|command| command.name == name)
}

/// Drop known mention tokens so a session title reads as the user's request.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, _) in text.match_indices('@') {
        if start < cursor || !starts_token(text, start) {
            continue;
        }
        let rest = &text[start + 1..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty()
            || !COMMANDS
                .iter()
                .any(|command| command.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        out.push_str(&text[cursor..start]);
        cursor = start + 1 + end;
    }
    out.push_str(&text[cursor..]);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A mention only counts at the start of the text or after whitespace, so an
/// email address such as `dev@arch.example` stays plain text.
fn starts_token(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_a_leading_or_inline_command() {
        assert!(has("@arch add a worker", "arch"));
        assert!(has("please @arch this change", "arch"));
        assert!(has("@ARCH add a worker", "arch"));
        assert_eq!(parse("fix it").len(), 0);
    }

    #[test]
    fn ignores_unknown_commands_and_addresses() {
        assert!(!has("dev@arch.example", "arch"));
        assert!(!has("@architecture review", "arch"));
        assert!(!has("@archived", "arch"));
        assert!(!has("@", "arch"));
    }

    #[test]
    fn reports_each_command_once() {
        let found = parse("@arch add it, @arch again");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "arch");
    }

    #[test]
    fn strips_commands_for_the_session_title() {
        assert_eq!(strip("@arch add a worker"), "add a worker");
        assert_eq!(strip("@worktree add a worker"), "add a worker");
        assert_eq!(strip("  @arch   add   a worker "), "add a worker");
        assert_eq!(strip("mail dev@arch.example"), "mail dev@arch.example");
    }

    #[test]
    fn recognizes_the_worktree_command() {
        assert!(has("@worktree fix login", "worktree"));
        assert!(has("@WORKTREE fix login", "worktree"));
        assert!(!has("@worktrees fix login", "worktree"));
    }
}
