use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::config::front_matter::split_front_matter;
use crate::contracts::{
    ConfigurationDiagnostic, ConfigurationKind, ConfigurationSource, DiagnosticSeverity,
    EffectiveConfiguration, Timestamp,
};

/// A file an adapter discovered, before it has been interpreted.
///
/// Bytes rather than a path: the loader does no IO, which is what lets
/// precedence and validation be tested without a temporary directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSource {
    /// Project-relative path, using `/` on every platform.
    pub relative_path: String,
    /// What kind of file it is.
    pub kind: ConfigurationKind,
    /// The file's bytes.
    pub content: Vec<u8>,
}

/// The task-specific choices that turn discovered definitions into an
/// effective configuration.
///
/// Discovery is an inventory. A nested `AGENTS.md` only becomes active for a
/// path in its subtree, and reusable definitions only become active when they
/// are explicitly selected. Keeping those choices out of filesystem discovery
/// makes resolution deterministic and easy to replay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationSelection {
    /// Project-relative file or directory the task is working on.
    pub active_path: Option<String>,
    /// Selected agent-profile identifier.
    pub agent_id: Option<String>,
    /// Selected workflow identifier.
    pub workflow_id: Option<String>,
    /// Selected skill identifiers, in explicit precedence order.
    pub skill_ids: Vec<String>,
}

/// Largest instruction file accepted, in bytes.
const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// Interpret discovered files into an ordered, validated configuration.
///
/// Never fails: a file that cannot be read becomes a disabled source and a
/// diagnostic. A broken file in one project must not make the application
/// unusable (`CONFIGURATION.md` §5).
#[must_use]
pub fn resolve(sources: Vec<RawSource>, resolved_at: Timestamp) -> EffectiveConfiguration {
    resolve_with_selection(sources, &ConfigurationSelection::default(), resolved_at)
}

/// Interpret discovered files for a particular task context.
#[must_use]
pub fn resolve_with_selection(
    sources: Vec<RawSource>,
    selection: &ConfigurationSelection,
    resolved_at: Timestamp,
) -> EffectiveConfiguration {
    let mut resolved = Vec::with_capacity(sources.len());
    let mut diagnostics = Vec::new();

    for raw in sources {
        resolved.push(interpret(raw, &mut diagnostics));
    }

    disable_duplicate_ids(&mut resolved, &mut diagnostics);
    apply_selection(&mut resolved, selection, &mut diagnostics);
    order(&mut resolved);

    EffectiveConfiguration::new(resolved, diagnostics, resolved_at)
}

/// Interpret one file.
fn interpret(
    raw: RawSource,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) -> ConfigurationSource {
    // Redacted before it is hashed or measured, so every record downstream
    // names content that may actually be released. A file with no credential
    // in it is byte-identical and hashes the same as it always did.
    //
    // A file that is not valid UTF-8 cannot be redacted, so it is hashed as
    // bytes — and it is also disabled below, so nothing will send it.
    let redacted = std::str::from_utf8(&raw.content)
        .ok()
        .map(crate::services::redact);
    let (releasable, redactions) = match &redacted {
        Some(result) => (result.text.as_bytes(), result.reason_codes.clone()),
        None => (raw.content.as_slice(), Vec::new()),
    };
    let byte_count = releasable.len() as u64;
    let content_hash = crate::contracts::configuration_hex(&Sha256::digest(releasable));

    let mut source = ConfigurationSource {
        source_id: raw.relative_path.clone(),
        relative_path: raw.relative_path.clone(),
        kind: raw.kind,
        declared_id: None,
        declared_name: None,
        scopes: Vec::new(),
        requested_capabilities: Vec::new(),
        priority: None,
        selection_order: None,
        enabled: true,
        active: false,
        precedence: raw.kind.precedence(),
        content_hash,
        byte_count,
        redactions,
        unknown_keys: Vec::new(),
    };

    if raw.content.len() > MAX_SOURCE_BYTES {
        diagnostics.push(error(
            &raw.relative_path,
            "file exceeds the 1 MiB size limit",
        ));
        source.enabled = false;
        return source;
    }

    // Instruction files are text. Invalid UTF-8 means the file is not what it
    // claims to be, so it is disabled rather than lossily decoded.
    let Some(redacted) = redacted else {
        diagnostics.push(error(&raw.relative_path, "file is not valid UTF-8"));
        source.enabled = false;
        return source;
    };

    // Front matter is parsed from the redacted text, which is what any later
    // reader will see. Parsing the original would let a declared id or scope
    // differ from the one in the file that actually gets used.
    match split_front_matter(&redacted.text) {
        Ok((matter, _body)) => {
            source.declared_id = matter.id.clone();
            source.declared_name = matter.name.clone();
            source.scopes = if raw.kind == ConfigurationKind::ScopedAgents {
                scoped_directory(&raw.relative_path).into_iter().collect()
            } else {
                matter.applies_to.clone()
            };
            source.requested_capabilities = matter.required_capabilities.clone();
            for capability in matter.preferred_capabilities {
                if !source.requested_capabilities.contains(&capability) {
                    source.requested_capabilities.push(capability);
                }
            }
            source.priority = if raw.kind == ConfigurationKind::Rule {
                matter.priority
            } else {
                None
            };
            source.unknown_keys = matter.unknown_keys.clone();
            // Absent `enabled` means enabled; the example files in this
            // repository set it to false deliberately.
            source.enabled = matter.enabled.unwrap_or(true);

            if source.scopes.iter().any(|scope| !valid_scope(scope)) {
                diagnostics.push(error(
                    &raw.relative_path,
                    "scope must be a non-empty project-relative portable glob without `..`",
                ));
                source.enabled = false;
            }

            for key in &matter.unknown_keys {
                diagnostics.push(warning(
                    &raw.relative_path,
                    format!("unrecognized front-matter key `{key}`; preserved but ignored"),
                ));
            }
        }
        Err(parse_error) => {
            diagnostics.push(error(
                &raw.relative_path,
                format!("could not read front matter: {parse_error}"),
            ));
            source.enabled = false;
        }
    }

    source
}

/// Decide which valid inventory items contribute to this task.
fn apply_selection(
    sources: &mut [ConfigurationSource],
    selection: &ConfigurationSelection,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) {
    let active_path = selection
        .active_path
        .as_deref()
        .filter(|path| valid_relative_path(path));
    if selection.active_path.is_some() && active_path.is_none() {
        diagnostics.push(error(
            "<selection>",
            "active path must be project-relative and cannot contain `..`",
        ));
    }

    let skill_order: HashMap<&str, u32> = selection
        .skill_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();

    for source in &mut *sources {
        if !source.enabled {
            source.active = false;
            continue;
        }

        let in_scope = scopes_match(&source.scopes, active_path);
        let selected = match source.kind {
            ConfigurationKind::Skill => {
                let order = source
                    .declared_id
                    .as_deref()
                    .and_then(|id| skill_order.get(id).copied());
                source.selection_order = order;
                order.is_some()
            }
            ConfigurationKind::AgentProfile => {
                source.declared_id.as_deref() == selection.agent_id.as_deref()
            }
            ConfigurationKind::Workflow => {
                source.declared_id.as_deref() == selection.workflow_id.as_deref()
            }
            ConfigurationKind::ScopedAgents => active_path.is_some(),
            ConfigurationKind::RootAgents
            | ConfigurationKind::Claude
            | ConfigurationKind::ProjectOverview
            | ConfigurationKind::Rule => true,
            ConfigurationKind::Unsupported => false,
        };
        source.active = selected && in_scope;
    }

    warn_missing_selection(
        sources,
        ConfigurationKind::AgentProfile,
        selection.agent_id.as_deref(),
        diagnostics,
    );
    warn_missing_selection(
        sources,
        ConfigurationKind::Workflow,
        selection.workflow_id.as_deref(),
        diagnostics,
    );
    for id in &selection.skill_ids {
        warn_missing_selection(sources, ConfigurationKind::Skill, Some(id), diagnostics);
    }
}

fn warn_missing_selection(
    sources: &[ConfigurationSource],
    kind: ConfigurationKind,
    selected_id: Option<&str>,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) {
    let Some(id) = selected_id else { return };
    if !sources
        .iter()
        .any(|source| source.kind == kind && source.declared_id.as_deref() == Some(id))
    {
        diagnostics.push(error(
            "<selection>",
            format!("selected {kind:?} id `{id}` was not found"),
        ));
    }
}

fn scoped_directory(relative_path: &str) -> Option<String> {
    relative_path
        .rsplit_once('/')
        .map(|(directory, _)| directory.to_owned())
}

fn valid_relative_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|component| component == ".." || component.is_empty())
}

fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && valid_relative_path(scope)
        && !scope
            .split('/')
            .any(|component| component == "." || component == "..")
}

fn scopes_match(scopes: &[String], active_path: Option<&str>) -> bool {
    if scopes.is_empty() {
        return true;
    }
    let Some(path) = active_path else {
        return false;
    };
    scopes.iter().any(|scope| {
        if scope.contains('*') || scope.contains('?') {
            glob_matches(scope, path)
        } else {
            path == scope
                || path
                    .strip_prefix(scope)
                    .is_some_and(|rest| rest.starts_with('/'))
        }
    })
}

/// Match the documented portable glob subset: `?` matches one non-separator,
/// `*` matches within one path component, and `**` crosses separators.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    let mut seen = HashSet::new();
    glob_matches_from(&pattern, &path, 0, 0, &mut seen)
}

fn glob_matches_from(
    pattern: &[char],
    path: &[char],
    pattern_index: usize,
    path_index: usize,
    seen: &mut HashSet<(usize, usize)>,
) -> bool {
    if !seen.insert((pattern_index, path_index)) {
        return false;
    }
    if pattern_index == pattern.len() {
        return path_index == path.len();
    }

    match pattern[pattern_index] {
        '*' if pattern.get(pattern_index + 1) == Some(&'*')
            && pattern.get(pattern_index + 2) == Some(&'/') =>
        {
            glob_matches_from(pattern, path, pattern_index + 3, path_index, seen)
                || (path_index < path.len()
                    && glob_matches_from(pattern, path, pattern_index, path_index + 1, seen))
        }
        '*' if pattern.get(pattern_index + 1) == Some(&'*') => {
            glob_matches_from(pattern, path, pattern_index + 2, path_index, seen)
                || (path_index < path.len()
                    && glob_matches_from(pattern, path, pattern_index, path_index + 1, seen))
        }
        '*' => {
            glob_matches_from(pattern, path, pattern_index + 1, path_index, seen)
                || (path
                    .get(path_index)
                    .is_some_and(|character| *character != '/')
                    && glob_matches_from(pattern, path, pattern_index, path_index + 1, seen))
        }
        '?' => {
            path.get(path_index)
                .is_some_and(|character| *character != '/')
                && glob_matches_from(pattern, path, pattern_index + 1, path_index + 1, seen)
        }
        literal => {
            path.get(path_index) == Some(&literal)
                && glob_matches_from(pattern, path, pattern_index + 1, path_index + 1, seen)
        }
    }
}

/// Disable every member of a duplicated `(kind, id)` pair.
///
/// `CONFIGURATION.md` §5: a duplicate identifier disables the affected item.
/// *All* of them are disabled rather than picking a winner — enumeration order
/// is not a decision procedure, and silently preferring one would make behavior
/// depend on the filesystem.
fn disable_duplicate_ids(
    sources: &mut [ConfigurationSource],
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) {
    let mut seen: HashMap<(ConfigurationKind, String), Vec<usize>> = HashMap::new();
    for (index, source) in sources.iter().enumerate() {
        if let Some(id) = &source.declared_id {
            seen.entry((source.kind, id.clone()))
                .or_default()
                .push(index);
        }
    }

    for ((kind, id), indices) in seen {
        if indices.len() < 2 {
            continue;
        }
        let paths: Vec<String> = indices
            .iter()
            .filter_map(|index| sources.get(*index).map(|s| s.relative_path.clone()))
            .collect();
        for index in indices {
            if let Some(source) = sources.get_mut(index) {
                source.enabled = false;
                diagnostics.push(error(
                    &source.relative_path,
                    format!(
                        "duplicate {kind:?} id `{id}`, also declared by {}; all copies disabled",
                        paths.join(", ")
                    ),
                ));
            }
        }
    }
}

/// Sort into precedence order: strongest first.
///
/// Ties break on path depth then path text, so the order is stable and does not
/// depend on how the filesystem happened to enumerate. A deeper `AGENTS.md` is
/// nearer the work and therefore stronger (`CONFIGURATION.md` §4).
fn order(sources: &mut [ConfigurationSource]) {
    sources.sort_by(|a, b| {
        a.precedence
            .cmp(&b.precedence)
            .then_with(|| {
                b.priority
                    .unwrap_or_default()
                    .cmp(&a.priority.unwrap_or_default())
            })
            .then_with(|| {
                a.selection_order
                    .unwrap_or(u32::MAX)
                    .cmp(&b.selection_order.unwrap_or(u32::MAX))
            })
            .then_with(|| depth(&b.relative_path).cmp(&depth(&a.relative_path)))
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

fn warning(path: &str, message: impl Into<String>) -> ConfigurationDiagnostic {
    ConfigurationDiagnostic {
        severity: DiagnosticSeverity::Warning,
        relative_path: path.to_owned(),
        message: message.into(),
    }
}

fn error(path: &str, message: impl Into<String>) -> ConfigurationDiagnostic {
    ConfigurationDiagnostic {
        severity: DiagnosticSeverity::Error,
        relative_path: path.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn raw(path: &str, kind: ConfigurationKind, content: &str) -> RawSource {
        RawSource {
            relative_path: path.to_owned(),
            kind,
            content: content.as_bytes().to_vec(),
        }
    }

    fn at() -> Timestamp {
        Timestamp::new("2026-01-01T00:00:00.000Z")
    }

    #[test]
    fn sources_are_ordered_strongest_precedence_first() {
        let resolved = resolve(
            vec![
                raw("CLAUDE.md", ConfigurationKind::Claude, "compat"),
                raw("AGENTS.md", ConfigurationKind::RootAgents, "root"),
                raw("src/AGENTS.md", ConfigurationKind::ScopedAgents, "scoped"),
                raw(
                    ".tramontana/project.md",
                    ConfigurationKind::ProjectOverview,
                    "overview",
                ),
            ],
            at(),
        );

        let paths: Vec<&str> = resolved
            .sources
            .iter()
            .map(|s| s.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "src/AGENTS.md",
                "AGENTS.md",
                ".tramontana/project.md",
                "CLAUDE.md"
            ]
        );
    }

    #[test]
    fn a_deeper_scoped_agents_file_outranks_a_shallower_one() {
        // "nearest applicable directory first" (`CONFIGURATION.md` §4).
        let resolved = resolve(
            vec![
                raw("a/AGENTS.md", ConfigurationKind::ScopedAgents, "shallow"),
                raw("a/b/c/AGENTS.md", ConfigurationKind::ScopedAgents, "deep"),
                raw("a/b/AGENTS.md", ConfigurationKind::ScopedAgents, "middle"),
            ],
            at(),
        );

        let paths: Vec<&str> = resolved
            .sources
            .iter()
            .map(|s| s.relative_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["a/b/c/AGENTS.md", "a/b/AGENTS.md", "a/AGENTS.md"]
        );
    }

    #[test]
    fn ordering_is_stable_and_not_filesystem_dependent() {
        // The same set in a different discovery order must resolve identically,
        // or behavior would depend on how a directory happened to enumerate.
        let files = || {
            vec![
                raw(
                    ".tramontana/rules/b.md",
                    ConfigurationKind::Rule,
                    "---\nid: b\n---\nb",
                ),
                raw(
                    ".tramontana/rules/a.md",
                    ConfigurationKind::Rule,
                    "---\nid: a\n---\na",
                ),
                raw("AGENTS.md", ConfigurationKind::RootAgents, "root"),
            ]
        };
        let forward = resolve(files(), at());
        let mut reversed = files();
        reversed.reverse();
        let backward = resolve(reversed, at());

        assert_eq!(forward.manifest_hash, backward.manifest_hash);
    }

    #[test]
    fn malformed_front_matter_disables_the_item_without_failing() {
        let resolved = resolve(
            vec![
                raw(
                    ".tramontana/skills/broken.md",
                    ConfigurationKind::Skill,
                    "---\nid: x\nunclosed",
                ),
                raw("AGENTS.md", ConfigurationKind::RootAgents, "fine"),
            ],
            at(),
        );

        let broken = resolved
            .sources
            .iter()
            .find(|s| s.relative_path.ends_with("broken.md"))
            .unwrap();
        assert!(!broken.enabled);
        assert!(resolved.has_errors());

        // The healthy file is unaffected.
        let fine = resolved
            .sources
            .iter()
            .find(|s| s.relative_path == "AGENTS.md")
            .unwrap();
        assert!(fine.enabled);
    }

    #[test]
    fn a_duplicate_id_disables_every_copy_rather_than_picking_one() {
        let resolved = resolve(
            vec![
                raw(
                    ".tramontana/rules/one.md",
                    ConfigurationKind::Rule,
                    "---\nid: dup\n---\nA",
                ),
                raw(
                    ".tramontana/rules/two.md",
                    ConfigurationKind::Rule,
                    "---\nid: dup\n---\nB",
                ),
            ],
            at(),
        );

        assert!(resolved.sources.iter().all(|s| !s.enabled));
        assert_eq!(
            resolved
                .diagnostics
                .iter()
                .filter(|d| d.severity == DiagnosticSeverity::Error)
                .count(),
            2
        );
    }

    #[test]
    fn the_same_id_in_different_kinds_is_not_a_duplicate() {
        // Ids are unique within a kind, not globally (`CONFIGURATION.md` §5).
        let resolved = resolve(
            vec![
                raw(
                    ".tramontana/rules/x.md",
                    ConfigurationKind::Rule,
                    "---\nid: shared\n---\nA",
                ),
                raw(
                    ".tramontana/skills/x.md",
                    ConfigurationKind::Skill,
                    "---\nid: shared\n---\nB",
                ),
            ],
            at(),
        );
        assert!(resolved.sources.iter().all(|s| s.enabled));
        assert!(!resolved.has_errors());
    }

    #[test]
    fn enabled_false_is_honored() {
        // The repository's own example files rely on this.
        let resolved = resolve(
            vec![raw(
                ".tramontana/skills/example.md",
                ConfigurationKind::Skill,
                "---\nid: example\nenabled: false\n---\nbody",
            )],
            at(),
        );
        assert!(!resolved.sources[0].enabled);
        // Being deliberately disabled is not an error.
        assert!(!resolved.has_errors());
    }

    #[test]
    fn invalid_utf8_disables_the_source() {
        let resolved = resolve(
            vec![RawSource {
                relative_path: "AGENTS.md".to_owned(),
                kind: ConfigurationKind::RootAgents,
                content: vec![0xff, 0xfe, 0x00],
            }],
            at(),
        );
        assert!(!resolved.sources[0].enabled);
        assert!(resolved.has_errors());
    }

    #[test]
    fn an_unknown_key_warns_but_keeps_the_source_enabled() {
        let resolved = resolve(
            vec![raw(
                ".tramontana/rules/x.md",
                ConfigurationKind::Rule,
                "---\nid: x\nfuture: 1\n---\nbody",
            )],
            at(),
        );
        assert!(resolved.sources[0].enabled);
        assert!(!resolved.has_errors());
        assert_eq!(resolved.sources[0].unknown_keys, vec!["future"]);
        assert_eq!(resolved.diagnostics.len(), 1);
        assert_eq!(
            resolved.diagnostics[0].severity,
            DiagnosticSeverity::Warning
        );
    }

    #[test]
    fn a_credential_in_an_instruction_file_is_redacted_before_it_is_hashed() {
        // The hash is what a context manifest records and what the release
        // boundary checks a payload against. If it named the original, a later
        // prompt assembly could send the credential and still satisfy every
        // record describing the release. Synthetic value.
        // Two shapes on purpose. The prose line is caught only by the key
        // prefix — the assignment rule requires a name with no whitespace, and
        // "Use OPENAI_API_KEY" is a sentence, not an assignment. The env line
        // is caught by both. That the narrow rule declines the prose is the
        // backstop behaving, not a miss.
        let raw = "# Rules\n\nUse OPENAI_API_KEY=sk-proj-A1b2C3d4E5f6G7h8I9j0K1l2M3n4 for calls.\n\
                   client_secret: s3cr3t-value\n";
        let resolved = resolve(
            vec![RawSource {
                relative_path: "AGENTS.md".to_owned(),
                kind: ConfigurationKind::RootAgents,
                content: raw.as_bytes().to_vec(),
            }],
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );

        let source = &resolved.sources[0];
        assert_eq!(
            source.redactions,
            vec![
                crate::services::redaction_reason::CREDENTIAL_ASSIGNMENT,
                crate::services::redaction_reason::PROVIDER_API_KEY,
            ]
        );
        // Not the hash of the file on disk.
        let raw_hash = crate::contracts::configuration_hex(&Sha256::digest(raw.as_bytes()));
        assert_ne!(source.content_hash, raw_hash);
        // But exactly the hash of what may be released.
        let releasable = crate::services::redact(raw).text;
        assert_eq!(
            source.content_hash,
            crate::contracts::configuration_hex(&Sha256::digest(releasable.as_bytes()))
        );
        assert_eq!(source.byte_count, releasable.len() as u64);
    }

    #[test]
    fn a_file_with_no_credential_hashes_exactly_as_it_always_did() {
        // Redaction must not churn every hash in every existing record.
        let raw = b"# Rules\n\nBe careful.\n";
        let resolved = resolve(
            vec![RawSource {
                relative_path: "AGENTS.md".to_owned(),
                kind: ConfigurationKind::RootAgents,
                content: raw.to_vec(),
            }],
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        assert_eq!(
            resolved.sources[0].content_hash,
            crate::contracts::configuration_hex(&Sha256::digest(raw))
        );
        assert!(resolved.sources[0].redactions.is_empty());
        assert_eq!(resolved.sources[0].byte_count, raw.len() as u64);
    }

    #[test]
    fn content_hashes_distinguish_otherwise_identical_files() {
        let a = resolve(
            vec![raw("AGENTS.md", ConfigurationKind::RootAgents, "one")],
            at(),
        );
        let b = resolve(
            vec![raw("AGENTS.md", ConfigurationKind::RootAgents, "two")],
            at(),
        );
        assert_ne!(a.sources[0].content_hash, b.sources[0].content_hash);
        assert_ne!(a.manifest_hash, b.manifest_hash);
    }

    #[test]
    fn only_nested_agents_files_which_contain_the_active_path_are_active() {
        let resolved = resolve_with_selection(
            vec![
                raw("AGENTS.md", ConfigurationKind::RootAgents, "root"),
                raw("apps/AGENTS.md", ConfigurationKind::ScopedAgents, "apps"),
                raw(
                    "apps/desktop/AGENTS.md",
                    ConfigurationKind::ScopedAgents,
                    "desktop",
                ),
                raw(
                    "crates/AGENTS.md",
                    ConfigurationKind::ScopedAgents,
                    "crates",
                ),
            ],
            &ConfigurationSelection {
                active_path: Some("apps/desktop/src/App.tsx".to_owned()),
                ..ConfigurationSelection::default()
            },
            at(),
        );

        let active: Vec<&str> = resolved
            .sources
            .iter()
            .filter(|source| source.active)
            .map(|source| source.relative_path.as_str())
            .collect();
        assert_eq!(
            active,
            vec!["apps/desktop/AGENTS.md", "apps/AGENTS.md", "AGENTS.md"]
        );
    }

    #[test]
    fn rules_use_priority_then_path_and_honor_portable_globs() {
        let resolved = resolve_with_selection(
            vec![
                raw(
                    ".tramontana/rules/low.md",
                    ConfigurationKind::Rule,
                    "---\nid: low\npriority: 1\napplies_to: [\"apps/**/*.rs\"]\n---\nlow",
                ),
                raw(
                    ".tramontana/rules/high.md",
                    ConfigurationKind::Rule,
                    "---\nid: high\npriority: 100\napplies_to: [\"apps/**/*.rs\"]\n---\nhigh",
                ),
                raw(
                    ".tramontana/rules/web.md",
                    ConfigurationKind::Rule,
                    "---\nid: web\npriority: 200\napplies_to: [\"apps/**/*.tsx\"]\n---\nweb",
                ),
            ],
            &ConfigurationSelection {
                active_path: Some("apps/main.rs".to_owned()),
                ..ConfigurationSelection::default()
            },
            at(),
        );

        assert_eq!(resolved.sources[0].declared_id.as_deref(), Some("web"));
        assert!(!resolved.sources[0].active);
        assert_eq!(resolved.sources[1].declared_id.as_deref(), Some("high"));
        assert!(resolved.sources[1].active);
        assert_eq!(resolved.sources[2].declared_id.as_deref(), Some("low"));
        assert!(resolved.sources[2].active);
    }

    #[test]
    fn reusable_definitions_are_inert_until_explicitly_selected() {
        let sources = vec![
            raw(
                ".tramontana/skills/rust.md",
                ConfigurationKind::Skill,
                "---\nid: rust\nrequired_capabilities: [filesystem.read]\n---\nbody",
            ),
            raw(
                ".tramontana/agents/coder.md",
                ConfigurationKind::AgentProfile,
                "---\nid: coder\npreferred_capabilities: [process.run]\n---\nbody",
            ),
        ];
        let inventory = resolve(sources.clone(), at());
        assert!(inventory.sources.iter().all(|source| !source.active));

        let selected = resolve_with_selection(
            sources,
            &ConfigurationSelection {
                agent_id: Some("coder".to_owned()),
                skill_ids: vec!["rust".to_owned()],
                ..ConfigurationSelection::default()
            },
            at(),
        );
        assert!(selected.sources.iter().all(|source| source.active));
        assert_eq!(
            selected.sources[0].requested_capabilities,
            vec!["filesystem.read"]
        );
        assert_eq!(
            selected.sources[1].requested_capabilities,
            vec!["process.run"]
        );
    }

    #[test]
    fn invalid_scope_and_selection_are_diagnostics_not_panics() {
        let resolved = resolve_with_selection(
            vec![raw(
                ".tramontana/rules/escape.md",
                ConfigurationKind::Rule,
                "---\nid: escape\napplies_to: [\"../outside/**\"]\n---\nbody",
            )],
            &ConfigurationSelection {
                active_path: Some("../outside/secret".to_owned()),
                skill_ids: vec!["missing".to_owned()],
                ..ConfigurationSelection::default()
            },
            at(),
        );
        assert!(!resolved.sources[0].enabled);
        assert!(!resolved.sources[0].active);
        assert!(resolved.has_errors());
        assert_eq!(resolved.diagnostics.len(), 3);
    }

    #[test]
    fn portable_globs_match_root_and_nested_paths() {
        assert!(glob_matches("**/*.rs", "main.rs"));
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(glob_matches("apps/*/src/**", "apps/desktop/src/App.tsx"));
        assert!(!glob_matches("apps/*/src/**", "crates/core/src/lib.rs"));
    }
}
