//! The person's skill library as folders on disk, and reading a skill to
//! import (ADR 0070).
//!
//! The library is `<app data>/skills/<id>/SKILL.md`, the layout Claude's
//! skills use, so a person can look at it, back it up, or copy a skill in by
//! hand. Only `SKILL.md` is ever read or written: a skill's other files —
//! scripts, references — are not imported, and nothing here runs anything.
//! Core decides what a skill is; this module only keeps files.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tramontana_core::ports::{RepositoryError, SkillFile, SkillLibrary};
use tramontana_core::services::is_skill_id;

/// The file every skill is.
pub const SKILL_FILE: &str = "SKILL.md";

/// The most read from any one `SKILL.md`. Core keeps less (64 KiB); this
/// only stops a huge file being read into memory to be refused.
const READ_LIMIT: u64 = 256 * 1024;

fn unavailable(error: impl std::fmt::Display) -> RepositoryError {
    RepositoryError::Unavailable(error.to_string())
}

/// Read at most [`READ_LIMIT`] bytes of text; `None` when it is longer or
/// not UTF-8.
fn read_limited(reader: impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    reader.take(READ_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > READ_LIMIT {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// A regular file, not a link to one elsewhere.
fn is_plain_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

/// A folder, not a link to one elsewhere.
fn is_plain_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_dir())
}

/// The person's skills, one folder each under `root`.
#[derive(Debug, Clone)]
pub struct FsSkillLibrary {
    root: PathBuf,
}

impl FsSkillLibrary {
    /// A library in `root`, created when the first skill is added.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Where the skills are kept.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder for `id`, or `None` when `id` could name anything else:
    /// only a skill id, which has no separators or dots, is joined on.
    fn folder(&self, id: &str) -> Option<PathBuf> {
        is_skill_id(id).then(|| self.root.join(id))
    }
}

#[async_trait]
impl SkillLibrary for FsSkillLibrary {
    async fn list(&self) -> Result<Vec<SkillFile>, RepositoryError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(unavailable(error)),
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let Ok(folder) = entry.file_name().into_string() else {
                continue;
            };
            let path = entry.path();
            // Links are not followed: a skill is what is in the library.
            if !is_plain_dir(&path) {
                continue;
            }
            let file = path.join(SKILL_FILE);
            if !is_plain_file(&file) {
                continue;
            }
            let Some(markdown) = fs::File::open(&file).ok().and_then(read_limited) else {
                continue;
            };
            found.push(SkillFile { folder, markdown });
        }
        found.sort_by(|left, right| left.folder.cmp(&right.folder));
        Ok(found)
    }

    async fn add(&self, file: &SkillFile) -> Result<(), RepositoryError> {
        let folder = self
            .folder(&file.folder)
            .ok_or_else(|| unavailable(format!("\"{}\" is not a skill name", file.folder)))?;
        fs::create_dir_all(&self.root).map_err(unavailable)?;
        // Creating the folder is the check: it fails if one is there.
        if let Err(error) = fs::create_dir(&folder) {
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                RepositoryError::Conflict {
                    kind: "skill",
                    id: file.folder.clone(),
                }
            } else {
                unavailable(error)
            });
        }
        // Written beside, then moved into place, so a half-written skill is
        // never listed.
        let partial = folder.join(".SKILL.md.partial");
        let written = fs::write(&partial, file.markdown.as_bytes())
            .and_then(|()| fs::rename(&partial, folder.join(SKILL_FILE)));
        if let Err(error) = written {
            let _ = fs::remove_dir_all(&folder);
            return Err(unavailable(error));
        }
        Ok(())
    }

    async fn remove(&self, id: &str) -> Result<(), RepositoryError> {
        let not_found = || RepositoryError::NotFound {
            kind: "skill",
            id: id.to_owned(),
        };
        let folder = self.folder(id).ok_or_else(not_found)?;
        // Only a real folder in the library: never a link's target.
        if !is_plain_dir(&folder) {
            return Err(not_found());
        }
        fs::remove_dir_all(&folder).map_err(unavailable)
    }
}

/// Why a skill could not be read for importing.
#[derive(Debug, thiserror::Error)]
pub enum SkillSourceError {
    /// Not a folder, `.zip` or `.md` file.
    #[error("choose a skill folder, a .zip file, or a .md file")]
    Unsupported,
    /// It has no `SKILL.md` where one is expected.
    #[error("there is no SKILL.md in it")]
    NoSkillFile,
    /// The `SKILL.md` is too long, or not text.
    #[error("its SKILL.md is too long or is not text")]
    Unreadable,
    /// It could not be opened.
    #[error("it could not be read: {0}")]
    Io(String),
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The `SKILL.md` in `directory`, matched without regard to case.
fn skill_file_in(directory: &Path) -> Option<PathBuf> {
    fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(SKILL_FILE))
        })
}

/// A skill to import: the name it came with (its folder or file), and its
/// `SKILL.md`. From a skill folder, a `.zip` holding one (at its top or one
/// folder down), or a single `.md` file.
///
/// # Errors
///
/// As [`SkillSourceError`].
pub fn read_skill_source(path: &Path) -> Result<(String, String), SkillSourceError> {
    let meta = fs::metadata(path).map_err(|error| SkillSourceError::Io(error.to_string()))?;
    if meta.is_dir() {
        let file = skill_file_in(path).ok_or(SkillSourceError::NoSkillFile)?;
        let reader =
            fs::File::open(file).map_err(|error| SkillSourceError::Io(error.to_string()))?;
        let markdown = read_limited(reader).ok_or(SkillSourceError::Unreadable)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok((name, markdown));
    }
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "md" | "markdown" => {
            let reader =
                fs::File::open(path).map_err(|error| SkillSourceError::Io(error.to_string()))?;
            let markdown = read_limited(reader).ok_or(SkillSourceError::Unreadable)?;
            Ok((stem(path), markdown))
        }
        "zip" => read_zipped_skill(path),
        _ => Err(SkillSourceError::Unsupported),
    }
}

/// The `SKILL.md` in a `.zip`. Only that entry is read; nothing is
/// extracted, so an archive's paths are never joined onto anything.
fn read_zipped_skill(path: &Path) -> Result<(String, String), SkillSourceError> {
    let file = fs::File::open(path).map_err(|error| SkillSourceError::Io(error.to_string()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| SkillSourceError::Io(error.to_string()))?;
    // The shallowest SKILL.md: at the top, or in one folder.
    let mut best: Option<(usize, usize, String)> = None;
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index_raw(index) else {
            continue;
        };
        let name = entry.name().replace('\\', "/");
        let parts: Vec<&str> = name.split('/').filter(|part| !part.is_empty()).collect();
        let Some(last) = parts.last() else {
            continue;
        };
        if !last.eq_ignore_ascii_case(SKILL_FILE) || parts.len() > 2 || entry.is_dir() {
            continue;
        }
        let folder = if parts.len() == 2 {
            parts[0].to_owned()
        } else {
            stem(path)
        };
        if best
            .as_ref()
            .is_none_or(|(depth, _, _)| parts.len() < *depth)
        {
            best = Some((parts.len(), index, folder));
        }
    }
    let (_, index, folder) = best.ok_or(SkillSourceError::NoSkillFile)?;
    let entry = archive
        .by_index(index)
        .map_err(|error| SkillSourceError::Io(error.to_string()))?;
    if entry.size() > READ_LIMIT {
        return Err(SkillSourceError::Unreadable);
    }
    let markdown = read_limited(entry).ok_or(SkillSourceError::Unreadable)?;
    Ok((folder, markdown))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good fixtures; a panic is the failure report"
)]
mod tests {
    use std::io::Write;

    use super::*;

    const SKILL: &str = "---\nname: code-review\n---\nRead the diff.";

    fn file(folder: &str) -> SkillFile {
        SkillFile {
            folder: folder.into(),
            markdown: SKILL.into(),
        }
    }

    #[tokio::test]
    async fn skills_are_added_listed_and_removed_as_folders() {
        let directory = tempfile::tempdir().unwrap();
        let library = FsSkillLibrary::new(directory.path().join("skills"));
        assert!(library.list().await.unwrap().is_empty());

        library.add(&file("code-review")).await.unwrap();
        library.add(&file("a-first")).await.unwrap();
        let listed = library.list().await.unwrap();
        assert_eq!(
            listed.iter().map(|f| f.folder.as_str()).collect::<Vec<_>>(),
            ["a-first", "code-review"]
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("skills/code-review/SKILL.md")).unwrap(),
            SKILL
        );

        assert!(matches!(
            library.add(&file("code-review")).await,
            Err(RepositoryError::Conflict { .. })
        ));
        library.remove("code-review").await.unwrap();
        assert!(!directory.path().join("skills/code-review").exists());
        assert!(matches!(
            library.remove("code-review").await,
            Err(RepositoryError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn nothing_outside_the_library_is_named_written_or_removed() {
        let directory = tempfile::tempdir().unwrap();
        let library = FsSkillLibrary::new(directory.path().join("skills"));
        fs::create_dir_all(directory.path().join("outside")).unwrap();
        assert!(library.add(&file("../outside")).await.is_err());
        assert!(matches!(
            library.remove("../outside").await,
            Err(RepositoryError::NotFound { .. })
        ));
        assert!(directory.path().join("outside").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_linked_folder_is_neither_listed_nor_removed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        let elsewhere = directory.path().join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join(SKILL_FILE), SKILL).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("linked")).unwrap();

        let library = FsSkillLibrary::new(root);
        assert!(library.list().await.unwrap().is_empty());
        assert!(library.remove("linked").await.is_err());
        assert!(elsewhere.join(SKILL_FILE).exists());
    }

    #[test]
    fn a_skill_is_read_from_a_folder_a_markdown_file_or_a_zip() {
        let directory = tempfile::tempdir().unwrap();

        let folder = directory.path().join("Release Notes");
        fs::create_dir_all(folder.join("scripts")).unwrap();
        fs::write(folder.join("skill.md"), SKILL).unwrap();
        fs::write(folder.join("scripts/run.sh"), "rm -rf /").unwrap();
        assert_eq!(
            read_skill_source(&folder).unwrap(),
            ("Release Notes".to_owned(), SKILL.to_owned())
        );

        let single = directory.path().join("tone.md");
        fs::write(&single, "Write plainly.").unwrap();
        assert_eq!(
            read_skill_source(&single).unwrap(),
            ("tone".to_owned(), "Write plainly.".to_owned())
        );

        let zipped = directory.path().join("bundle.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zipped).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("pdf-forms/scripts/fill.py", options)
            .unwrap();
        writer.write_all(b"print()").unwrap();
        writer.start_file("pdf-forms/SKILL.md", options).unwrap();
        writer.write_all(SKILL.as_bytes()).unwrap();
        writer.finish().unwrap();
        assert_eq!(
            read_skill_source(&zipped).unwrap(),
            ("pdf-forms".to_owned(), SKILL.to_owned())
        );
    }

    #[test]
    fn a_source_without_a_skill_or_of_another_kind_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_skill_source(directory.path()),
            Err(SkillSourceError::NoSkillFile)
        ));
        let other = directory.path().join("notes.txt");
        fs::write(&other, "x").unwrap();
        assert!(matches!(
            read_skill_source(&other),
            Err(SkillSourceError::Unsupported)
        ));
        let big = directory.path().join("big.md");
        fs::write(&big, "x".repeat(300 * 1024)).unwrap();
        assert!(matches!(
            read_skill_source(&big),
            Err(SkillSourceError::Unreadable)
        ));
    }
}
