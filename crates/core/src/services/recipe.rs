//! Configured recipes: the project's own commands, resolved by the core so a
//! person approves exactly what runs (ADR 0025).
//!
//! A model names a recipe and nothing else. The core looks the name up in the
//! configuration the task started with, and the approval prompt and the tool
//! receive the same resolved command: the argument list from
//! `.tramontana/project.md`, the project root as working directory, and the
//! environment names allowed here. No tool resolves a recipe itself, so what
//! runs cannot differ from what was approved.

use serde_json::{Value, json};

use crate::contracts::{ApprovalDetail, EffectiveConfiguration, ErrorCode};

/// The one tool that runs recipes.
pub const RECIPE_TOOL_ID: &str = "builtin.recipe.run";

/// Variables a project command may inherit, when they are set: enough to find
/// programs, a home, a locale and a temporary directory. None is meant to
/// carry a credential; anything else must be passed deliberately.
///
/// Defined here, not beside the command broker, because the approval prompt
/// names them: what a person is shown and what the command receives must come
/// from one list.
pub const COMMAND_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TMP",
    "TEMP",
    // Windows needs these to start most programs at all.
    "SystemRoot",
    "windir",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
];

/// A recipe call, resolved: what the tool runs, and what the person is shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedRecipe {
    /// The tool's arguments: the recipe id and exactly the command to run.
    pub(crate) arguments: Value,
    /// The facts the approval prompt shows.
    pub(crate) details: Vec<ApprovalDetail>,
}

/// Resolve `{ "recipe": id }` against the configuration the task started with.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] when no recipe is named, and
/// [`ErrorCode::NotFound`] when the task has no configuration or its project
/// declares no such recipe. Either way nobody is asked and nothing runs.
///
/// `local_only` is whether the task routes Local Only, which the prompt
/// mentions because the mode does not reach commands.
pub(crate) fn resolve(
    configuration: Option<&EffectiveConfiguration>,
    arguments: &Value,
    local_only: bool,
) -> Result<ResolvedRecipe, ErrorCode> {
    let id = arguments
        .get("recipe")
        .and_then(Value::as_str)
        .ok_or(ErrorCode::InvalidRequest)?;
    let (recipe, source) = configuration
        .and_then(|configuration| configuration.recipe(id))
        .ok_or(ErrorCode::NotFound)?;
    Ok(ResolvedRecipe {
        arguments: json!({
            "recipe": id,
            "run": recipe.run,
            "inherit": COMMAND_ENVIRONMENT,
            "declaredIn": source.relative_path,
        }),
        details: vec![
            detail("Recipe", id),
            detail("Command", &display(&recipe.run)),
            detail("Directory", "the project root"),
            detail(
                "Environment",
                &format!("cleared, then {} where set", COMMAND_ENVIRONMENT.join(", ")),
            ),
            // Tramontana has no network control over commands, so the person
            // is told rather than left to assume (EXECUTION_BOUNDARY.md).
            detail("Network", network(local_only)),
            detail("Declared in", &source.relative_path),
        ],
    })
}

/// What the prompt says about the network. Under Local Only it also says the
/// mode does not reach commands, since that is exactly when someone might
/// assume it does.
fn network(local_only: bool) -> &'static str {
    if local_only {
        "not restricted: this command can reach the network. Local Only governs where models run, not commands."
    } else {
        "not restricted: this command can reach the network."
    }
}

fn detail(label: &str, value: &str) -> ApprovalDetail {
    ApprovalDetail {
        label: label.to_owned(),
        value: value.to_owned(),
    }
}

/// An argument list as a person would read it.
///
/// For display only: nothing is ever run through a shell. An argument that
/// would be ambiguous written bare — empty, or holding spaces, quotes or shell
/// punctuation — is shown in single quotes, so the prompt never makes two
/// arguments look like one, or one look like two.
pub(crate) fn display(run: &[String]) -> String {
    run.iter()
        .map(|argument| {
            let plain = !argument.is_empty()
                && argument
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
            if plain {
                argument.clone()
            } else {
                format!("'{}'", argument.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::config::{RawSource, resolve as resolve_configuration};
    use crate::contracts::{ConfigurationKind, Timestamp};

    fn configuration(front_matter: &str) -> EffectiveConfiguration {
        resolve_configuration(
            vec![RawSource {
                relative_path: ".tramontana/project.md".to_owned(),
                kind: ConfigurationKind::ProjectOverview,
                content: format!("---\n{front_matter}\n---\n# P\n").into_bytes(),
            }],
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        )
    }

    #[test]
    fn a_named_recipe_resolves_to_exactly_its_declared_command() {
        let configuration = configuration("id: p\nrecipes:\n  test: [cargo, test, --workspace]");

        let resolved = resolve(Some(&configuration), &json!({ "recipe": "test" }), false).unwrap();

        assert_eq!(
            resolved.arguments["run"],
            json!(["cargo", "test", "--workspace"])
        );
        assert_eq!(resolved.arguments["inherit"], json!(COMMAND_ENVIRONMENT));
        assert_eq!(resolved.arguments["declaredIn"], ".tramontana/project.md");
        let shown: Vec<(&str, &str)> = resolved
            .details
            .iter()
            .map(|detail| (detail.label.as_str(), detail.value.as_str()))
            .collect();
        assert!(
            shown.contains(&("Command", "cargo test --workspace")),
            "{shown:?}"
        );
        assert!(
            shown.contains(&("Declared in", ".tramontana/project.md")),
            "{shown:?}"
        );
    }

    #[test]
    fn the_prompt_says_the_network_is_not_restricted_and_what_local_only_covers() {
        let configuration = configuration("id: p\nrecipes:\n  test: [cargo, test]");
        let network = |local_only| {
            resolve(
                Some(&configuration),
                &json!({ "recipe": "test" }),
                local_only,
            )
            .unwrap()
            .details
            .into_iter()
            .find(|detail| detail.label == "Network")
            .map(|detail| detail.value)
        };

        // Every recipe prompt mentions the network, in either mode.
        let anywhere = network(false).unwrap();
        assert!(anywhere.contains("can reach the network"), "{anywhere}");
        assert!(!anywhere.contains("Local Only"), "{anywhere}");

        let local = network(true).unwrap();
        assert!(local.contains("can reach the network"), "{local}");
        assert!(
            local.contains("Local Only governs where models run, not commands"),
            "{local}"
        );
    }

    #[test]
    fn nothing_resolves_without_a_declared_recipe_or_a_configuration() {
        let configuration = configuration("id: p\nrecipes:\n  test: [cargo, test]");
        assert_eq!(
            resolve(Some(&configuration), &json!({ "recipe": "deploy" }), false),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            resolve(None, &json!({ "recipe": "test" }), false),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            resolve(Some(&configuration), &json!({}), false),
            Err(ErrorCode::InvalidRequest)
        );
    }

    #[test]
    fn the_prompt_never_blurs_where_one_argument_ends() {
        let run = |items: &[&str]| {
            items
                .iter()
                .map(|&item| item.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            display(&run(&["cargo", "test", "--jobs=4"])),
            "cargo test --jobs=4"
        );
        assert_eq!(display(&run(&["echo", "a b"])), "echo 'a b'");
        assert_eq!(display(&run(&["echo", ""])), "echo ''");
        assert_eq!(display(&run(&["echo", "it's"])), r"echo 'it'\''s'");
        assert_eq!(display(&run(&["rm", "-rf", "$HOME;"])), "rm -rf '$HOME;'");
    }
}
