//! An App's tools, as tools the registry can hold, or refused with a reason.

use std::collections::HashMap;

use tramontana_core::contracts::{ReplaySafety, RiskClass, SideEffect, ToolDescriptor};
use tramontana_core::services::{ToolInputSchema, redact};

/// The longest tool name an App may use.
pub(crate) const MAX_NAME: usize = 64;
/// The longest description kept, in characters; the rest is cut.
pub(crate) const MAX_DESCRIPTION: usize = 1_000;
/// The longest title kept, in characters.
pub(crate) const MAX_TITLE: usize = 80;

/// A tool an App offered that Tramontana will not register.
///
/// The name is the App's own, redacted and cut to length: it is shown to the
/// person as diagnostics, and a hostile App chooses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The name the App gave, as safe to show.
    pub name: String,
    /// Why it was refused, in words.
    pub reason: &'static str,
}

/// A tool the registry can hold, with what calling it needs.
pub(crate) struct AppTool {
    pub(crate) descriptor: ToolDescriptor,
    /// The name the App knows it by, which is what a call must send.
    pub(crate) name: String,
    pub(crate) input: ToolInputSchema,
    pub(crate) output: Option<ToolInputSchema>,
}

impl std::fmt::Debug for AppTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppTool")
            .field("tool_id", &self.descriptor.tool_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// What an App offered, split into what can be registered and what cannot.
#[derive(Debug, Default)]
pub(crate) struct Discovered {
    pub(crate) tools: Vec<AppTool>,
    pub(crate) refused: Vec<Refusal>,
}

/// Map `offered` into tools under `mcp.<app_id>.`.
///
/// A name is made lowercase and a dot becomes `_`, because tool identifiers
/// allow neither (`ToolRegistry`). Two names that become the same are both
/// refused rather than one winning, as the registry refuses a duplicate. A
/// name with any other character outside what MCP allows is refused rather
/// than rewritten: a name like `../escape` has no honest mapping.
pub(crate) fn discover(app_id: &str, offered: Vec<rmcp::model::Tool>) -> Discovered {
    let mut discovered = Discovered::default();
    let mut candidates = Vec::new();
    for tool in offered {
        let name = tool.name.to_string();
        match mapped_name(&name) {
            Ok(mapped) => candidates.push((mapped, tool)),
            Err(reason) => discovered.refused.push(Refusal {
                name: shown(&name, MAX_NAME),
                reason,
            }),
        }
    }

    let mut uses: HashMap<String, usize> = HashMap::new();
    for (mapped, _) in &candidates {
        *uses.entry(mapped.clone()).or_default() += 1;
    }
    for (mapped, tool) in candidates {
        let name = tool.name.to_string();
        if uses.get(&mapped).copied().unwrap_or(0) > 1 {
            discovered.refused.push(Refusal {
                name: shown(&name, MAX_NAME),
                reason: "its name is the same as another tool's once made lowercase",
            });
            continue;
        }
        match app_tool(app_id, &mapped, tool) {
            Ok(tool) => discovered.tools.push(tool),
            Err(reason) => discovered.refused.push(Refusal {
                name: shown(&name, MAX_NAME),
                reason,
            }),
        }
    }
    discovered
}

fn mapped_name(name: &str) -> Result<String, &'static str> {
    if name.is_empty() {
        return Err("it has no name");
    }
    if name.chars().count() > MAX_NAME {
        return Err("its name is longer than 64 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err("its name has characters MCP does not allow");
    }
    Ok(name
        .chars()
        .map(|c| {
            if c == '.' {
                '_'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect())
}

fn app_tool(app_id: &str, mapped: &str, tool: rmcp::model::Tool) -> Result<AppTool, &'static str> {
    let input_schema = serde_json::Value::Object((*tool.input_schema).clone());
    let input = ToolInputSchema::compile(&input_schema)
        .map_err(|_| "its input schema uses what Tramontana cannot check")?;
    let output = match &tool.output_schema {
        None => None,
        Some(schema) => Some(
            ToolInputSchema::compile(&serde_json::Value::Object((**schema).clone()))
                .map_err(|_| "its output schema uses what Tramontana cannot check")?,
        ),
    };

    // What the App says about itself. A hint, never policy: the permission
    // path for App tools does not trust it (ADR 0035).
    let reads_only = tool
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.read_only_hint)
        == Some(true);
    let name = tool.name.to_string();
    let title = tool.title.clone().unwrap_or_else(|| name.clone());
    let description = tool
        .description
        .as_deref()
        .map(|text| shown(text, MAX_DESCRIPTION))
        .unwrap_or_default();

    let provider = format!("mcp.{app_id}");
    Ok(AppTool {
        descriptor: ToolDescriptor {
            tool_id: format!("{provider}.{mapped}"),
            provider_id: provider.clone(),
            display_name: shown(&title, MAX_TITLE),
            description,
            input_schema,
            risk_class: if reads_only {
                RiskClass::Read
            } else {
                RiskClass::Dangerous
            },
            side_effect: if reads_only {
                SideEffect::None
            } else {
                SideEffect::External
            },
            capability: provider,
            supports_cancellation: true,
            // Tramontana cannot see what an App does, so it cannot say that
            // doing it again is harmless.
            replay_safety: ReplaySafety::Unknown,
        },
        name,
        input,
        output,
    })
}

/// Untrusted text made safe to show: redacted, then cut to `limit`
/// characters.
pub(crate) fn shown(text: &str, limit: usize) -> String {
    let redacted = redact(text).text;
    if redacted.chars().count() <= limit {
        return redacted;
    }
    let mut cut: String = redacted.chars().take(limit).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn tool(name: &str, schema: serde_json::Value) -> rmcp::model::Tool {
        let schema = schema.as_object().cloned().unwrap();
        rmcp::model::Tool::new(name.to_owned(), "a tool", Arc::new(schema))
    }

    fn object() -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    #[test]
    fn a_tool_is_namespaced_under_its_app() {
        let found = discover("notes", vec![tool("Read.Page", object())]);
        assert!(found.refused.is_empty(), "{:?}", found.refused);
        assert_eq!(found.tools[0].descriptor.tool_id, "mcp.notes.read_page");
        assert_eq!(found.tools[0].descriptor.provider_id, "mcp.notes");
        // The call still uses the App's own name.
        assert_eq!(found.tools[0].name, "Read.Page");
    }

    #[test]
    fn names_that_collide_once_lowercased_are_both_refused() {
        let found = discover(
            "notes",
            vec![tool("Search", object()), tool("search", object())],
        );
        assert!(found.tools.is_empty());
        assert_eq!(found.refused.len(), 2);
    }

    #[test]
    fn a_name_that_could_leave_its_namespace_is_refused_not_rewritten() {
        let found = discover("notes", vec![tool("../escape", object())]);
        assert!(found.tools.is_empty());
        assert_eq!(
            found.refused[0].reason,
            "its name has characters MCP does not allow"
        );
    }

    #[test]
    fn a_schema_the_core_cannot_check_is_refused() {
        let found = discover(
            "notes",
            vec![tool(
                "linked",
                serde_json::json!({"type": "object", "properties": {"a": {"$ref": "#/x"}}}),
            )],
        );
        assert!(found.tools.is_empty());
        assert_eq!(
            found.refused[0].reason,
            "its input schema uses what Tramontana cannot check"
        );
    }

    #[test]
    fn a_description_is_cut_to_length() {
        let mut long = tool("wordy", object());
        long.description = Some("d".repeat(10_000).into());
        let found = discover("notes", vec![long]);
        assert_eq!(
            found.tools[0].descriptor.description.chars().count(),
            MAX_DESCRIPTION + 1
        );
    }

    #[test]
    fn nothing_an_app_says_makes_its_tool_safe_to_replay() {
        let found = discover("notes", vec![tool("anything", object())]);
        assert_eq!(
            found.tools[0].descriptor.replay_safety,
            ReplaySafety::Unknown
        );
    }
}
