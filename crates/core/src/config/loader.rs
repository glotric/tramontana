use std::collections::HashMap;

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

/// Largest instruction file accepted, in bytes.
const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// Interpret discovered files into an ordered, validated configuration.
///
/// Never fails: a file that cannot be read becomes a disabled source and a
/// diagnostic. A broken file in one project must not make the application
/// unusable (`CONFIGURATION.md` §5).
#[must_use]
pub fn resolve(sources: Vec<RawSource>, resolved_at: Timestamp) -> EffectiveConfiguration {
    let mut resolved = Vec::with_capacity(sources.len());
    let mut diagnostics = Vec::new();

    for raw in sources {
        resolved.push(interpret(raw, &mut diagnostics));
    }

    disable_duplicate_ids(&mut resolved, &mut diagnostics);
    order(&mut resolved);

    EffectiveConfiguration::new(resolved, diagnostics, resolved_at)
}

/// Interpret one file.
fn interpret(
    raw: RawSource,
    diagnostics: &mut Vec<ConfigurationDiagnostic>,
) -> ConfigurationSource {
    let byte_count = raw.content.len() as u64;
    let content_hash = crate::contracts::configuration_hex(&Sha256::digest(&raw.content));

    let mut source = ConfigurationSource {
        source_id: raw.relative_path.clone(),
        relative_path: raw.relative_path.clone(),
        kind: raw.kind,
        declared_id: None,
        declared_name: None,
        enabled: true,
        precedence: raw.kind.precedence(),
        content_hash,
        byte_count,
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
    let Ok(text) = std::str::from_utf8(&raw.content) else {
        diagnostics.push(error(&raw.relative_path, "file is not valid UTF-8"));
        source.enabled = false;
        return source;
    };

    match split_front_matter(text) {
        Ok((matter, _body)) => {
            source.declared_id = matter.id.clone();
            source.declared_name = matter.name.clone();
            source.unknown_keys = matter.unknown_keys.clone();
            // Absent `enabled` means enabled; the example files in this
            // repository set it to false deliberately.
            source.enabled = matter.enabled.unwrap_or(true);

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
}
