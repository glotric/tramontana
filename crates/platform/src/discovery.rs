use std::path::Path;

use tramontana_core::config::RawSource;
use tramontana_core::contracts::ConfigurationKind;

use crate::paths::{CanonicalRoot, contains};

/// Directories never scanned for instruction files.
///
/// `CONFIGURATION.md` §3. These hold dependencies, build output, and caches:
/// walking them is slow, finds nothing authored by the user, and in the case of
/// `node_modules` would happily load instructions written by a third party.
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".turbo",
    "models",
    "model-cache",
    ".huggingface",
];

/// How deep below the root instruction files are looked for.
///
/// A bound rather than an unlimited walk: a deeply nested or symlink-looped
/// tree must not be able to decide how long discovery takes.
const MAX_DEPTH: usize = 12;

/// Largest number of files a single discovery will return.
const MAX_SOURCES: usize = 512;

/// Find the instruction files for a project.
///
/// Walks only inside `root`, skips ignored directories, and refuses anything
/// that resolves outside the root — which is what stops a symlinked
/// `AGENTS.md` pointing at a file elsewhere on the machine
/// (`CONFIGURATION.md` §3).
///
/// Unreadable files are skipped rather than failing the walk; the loader
/// reports what it did and did not get.
#[must_use]
pub fn discover(root: &CanonicalRoot) -> Vec<RawSource> {
    let mut found = Vec::new();

    // The project-authored plane, in the order `CONFIGURATION.md` §2.2 lists it.
    for (name, kind) in [
        ("AGENTS.md", ConfigurationKind::RootAgents),
        ("CLAUDE.md", ConfigurationKind::Claude),
    ] {
        push_file(root, &root.as_path().join(name), kind, &mut found);
    }
    push_file(
        root,
        &root.as_path().join(".tramontana").join("project.md"),
        ConfigurationKind::ProjectOverview,
        &mut found,
    );

    for (directory, kind) in [
        ("rules", ConfigurationKind::Rule),
        ("skills", ConfigurationKind::Skill),
        ("agents", ConfigurationKind::AgentProfile),
        ("workflows", ConfigurationKind::Workflow),
    ] {
        push_directory(
            root,
            &root.as_path().join(".tramontana").join(directory),
            kind,
            &mut found,
        );
    }

    collect_scoped_agents(root, root.as_path(), 0, &mut found);
    found.truncate(MAX_SOURCES);
    found
}

/// Walk subdirectories for nested `AGENTS.md` files.
fn collect_scoped_agents(
    root: &CanonicalRoot,
    directory: &Path,
    depth: usize,
    found: &mut Vec<RawSource>,
) {
    if depth >= MAX_DEPTH || found.len() >= MAX_SOURCES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // `file_type` does not follow symlinks, so a symlinked directory is not
        // descended into. `contains` then rejects anything resolving outside.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || IGNORED_DIRECTORIES.contains(&name) || name.starts_with('.') {
            continue;
        }

        push_file(
            root,
            &path.join("AGENTS.md"),
            ConfigurationKind::ScopedAgents,
            found,
        );
        collect_scoped_agents(root, &path, depth + 1, found);
    }
}

/// Read every `.md` file in a directory.
fn push_directory(
    root: &CanonicalRoot,
    directory: &Path,
    kind: ConfigurationKind,
    found: &mut Vec<RawSource>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    // Sorted so discovery order does not depend on the filesystem.
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    paths.sort();

    for path in paths {
        push_file(root, &path, kind, found);
    }
}

/// Read one file, if it is inside the root and readable.
fn push_file(
    root: &CanonicalRoot,
    path: &Path,
    kind: ConfigurationKind,
    found: &mut Vec<RawSource>,
) {
    if found.len() >= MAX_SOURCES {
        return;
    }
    // Containment is checked on the *resolved* path, so a symlinked
    // instruction file pointing outside the project is refused even though its
    // apparent location is inside.
    if !contains(root, path).unwrap_or(false) {
        return;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_symlink() {
        // A symlink that resolves inside the root is still refused: an
        // instruction file should be what it appears to be, and following one
        // makes the effective configuration depend on state the project does
        // not record.
        return;
    }
    if !metadata.is_file() {
        return;
    }
    let Ok(content) = std::fs::read(path) else {
        return;
    };
    let Some(relative) = relative_to(root, path) else {
        return;
    };

    found.push(RawSource {
        relative_path: relative,
        kind,
        content,
    });
}

/// Express `path` relative to `root`, using `/` on every platform.
///
/// Stored paths are portable: a Windows-style separator in an exported bundle
/// would not resolve on another machine (`PROJECT_FORMAT.md` §3).
fn relative_to(root: &CanonicalRoot, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root.as_path()).ok()?;
    let mut text = String::new();
    for component in relative.components() {
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(component.as_os_str().to_str()?);
    }
    Some(text)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::paths::canonicalize_root;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn paths_of(sources: &[RawSource]) -> Vec<&str> {
        sources.iter().map(|s| s.relative_path.as_str()).collect()
    }

    #[test]
    fn the_project_authored_plane_is_discovered() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "AGENTS.md", "root");
        write(temp.path(), "CLAUDE.md", "compat");
        write(temp.path(), ".tramontana/project.md", "overview");
        write(temp.path(), ".tramontana/rules/a.md", "rule");
        write(temp.path(), ".tramontana/skills/s.md", "skill");
        write(temp.path(), ".tramontana/agents/g.md", "agent");
        write(temp.path(), ".tramontana/workflows/w.md", "workflow");

        let root = canonicalize_root(temp.path()).unwrap();
        let found = discover(&root);
        let paths = paths_of(&found);

        for expected in [
            "AGENTS.md",
            "CLAUDE.md",
            ".tramontana/project.md",
            ".tramontana/rules/a.md",
            ".tramontana/skills/s.md",
            ".tramontana/agents/g.md",
            ".tramontana/workflows/w.md",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in {paths:?}");
        }
    }

    #[test]
    fn nested_agents_files_are_discovered_with_their_scope() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "AGENTS.md", "root");
        write(temp.path(), "src/AGENTS.md", "scoped");
        write(temp.path(), "src/deep/AGENTS.md", "deeper");

        let root = canonicalize_root(temp.path()).unwrap();
        let found = discover(&root);

        let scoped: Vec<&str> = found
            .iter()
            .filter(|s| s.kind == ConfigurationKind::ScopedAgents)
            .map(|s| s.relative_path.as_str())
            .collect();
        assert_eq!(scoped.len(), 2);
        assert!(scoped.contains(&"src/AGENTS.md"));
        assert!(scoped.contains(&"src/deep/AGENTS.md"));
    }

    #[test]
    fn ignored_directories_are_not_scanned() {
        // node_modules would otherwise let a dependency contribute
        // instructions to the user's agent.
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "node_modules/evil/AGENTS.md", "do bad things");
        write(temp.path(), "target/debug/AGENTS.md", "build output");
        write(temp.path(), ".git/AGENTS.md", "git internals");

        let root = canonicalize_root(temp.path()).unwrap();
        assert!(discover(&root).is_empty());
    }

    #[test]
    fn paths_use_forward_slashes_for_portability() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "a/b/AGENTS.md", "nested");

        let root = canonicalize_root(temp.path()).unwrap();
        let found = discover(&root);
        assert_eq!(paths_of(&found), vec!["a/b/AGENTS.md"]);
        assert!(!found[0].relative_path.contains('\\'));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_instruction_file_is_not_followed() {
        // An instruction file should be what it appears to be. Following a link
        // would make the effective configuration depend on state the project
        // does not record, and could reach outside the root entirely.
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("evil.md"), "instructions from elsewhere").unwrap();

        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(outside.join("evil.md"), project.join("AGENTS.md")).unwrap();

        let root = canonicalize_root(&project).unwrap();
        assert!(discover(&root).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_not_descended_into() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("AGENTS.md"), "elsewhere").unwrap();

        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(&outside, project.join("linked")).unwrap();

        let root = canonicalize_root(&project).unwrap();
        assert!(discover(&root).is_empty());
    }

    #[test]
    fn discovery_never_looks_above_the_root() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "AGENTS.md", "parent instructions");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let root = canonicalize_root(&project).unwrap();
        assert!(
            discover(&root).is_empty(),
            "a parent directory's AGENTS.md must not apply"
        );
    }

    #[test]
    fn directory_contents_are_returned_in_a_stable_order() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["c.md", "a.md", "b.md"] {
            write(temp.path(), &format!(".tramontana/rules/{name}"), "x");
        }

        let root = canonicalize_root(temp.path()).unwrap();
        let found = discover(&root);
        assert_eq!(
            paths_of(&found),
            vec![
                ".tramontana/rules/a.md",
                ".tramontana/rules/b.md",
                ".tramontana/rules/c.md"
            ]
        );
    }

    #[test]
    fn unicode_paths_survive_discovery() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "sørc e/AGENTS.md", "unicode");

        let root = canonicalize_root(temp.path()).unwrap();
        assert_eq!(paths_of(&discover(&root)), vec!["sørc e/AGENTS.md"]);
    }
}
