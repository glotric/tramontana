//! What the model guide can tell the person about this computer: whether
//! `llama-server` is where installers usually put it, and which model files
//! are in the folders people usually download them to.
//!
//! Only looks: it runs nothing, downloads nothing, and searches no `PATH`.
//! The person still chooses both files in the native picker; this only tells
//! them where to look.

use std::path::{Path, PathBuf};

/// The most model files listed.
const MODEL_FILES: usize = 20;

/// A model file found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FoundModel {
    pub(crate) path: String,
    pub(crate) bytes: u64,
}

/// What the guide found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelSetupHints {
    /// `llama-server`, where an installer usually puts it.
    pub(crate) llama_server: Option<String>,
    /// `.gguf` files in the usual download folders, largest first.
    pub(crate) models: Vec<FoundModel>,
    /// The folders that were looked in, so the guide can say so.
    pub(crate) looked_in: Vec<String>,
    /// This computer's operating system: `macos`, `windows` or `linux`.
    pub(crate) platform: &'static str,
}

/// Where installers usually put `llama-server` on this platform.
fn server_candidates(home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        let mut found = Vec::new();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            found.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("WinGet")
                    .join("Links")
                    .join("llama-server.exe"),
            );
        }
        found.push(home.join("llama.cpp").join("llama-server.exe"));
        found
    } else {
        vec![
            PathBuf::from("/opt/homebrew/bin/llama-server"),
            PathBuf::from("/usr/local/bin/llama-server"),
            PathBuf::from("/usr/bin/llama-server"),
            home.join(".local").join("bin").join("llama-server"),
        ]
    }
}

/// The folders people usually download model files to.
fn model_folders(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("models"),
        home.join("Downloads"),
        home.join(".cache").join("llama.cpp"),
        home.join("Library").join("Caches").join("llama.cpp"),
    ]
}

/// What is in `home`'s usual places, without running or opening anything.
pub(crate) fn find(home: &Path) -> ModelSetupHints {
    let llama_server = server_candidates(home)
        .into_iter()
        .find(|path| path.is_file())
        .map(|path| path.display().to_string());

    let folders = model_folders(home);
    let mut models = Vec::new();
    for folder in &folders {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_gguf = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"));
            let Ok(meta) = entry.metadata() else { continue };
            if is_gguf && meta.is_file() {
                models.push(FoundModel {
                    path: path.display().to_string(),
                    bytes: meta.len(),
                });
            }
        }
    }
    models.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    models.truncate(MODEL_FILES);

    ModelSetupHints {
        llama_server,
        models,
        looked_in: folders
            .iter()
            .filter(|folder| folder.is_dir())
            .map(|folder| folder.display().to_string())
            .collect(),
        platform: if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(windows) {
            "windows"
        } else {
            "linux"
        },
    }
}

/// The person's home folder, if the environment names one.
pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good fixtures; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn model_files_in_the_usual_folders_are_listed_largest_first() {
        let home = tempfile::tempdir().unwrap();
        let models = home.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("small.gguf"), vec![0_u8; 10]).unwrap();
        std::fs::write(models.join("big.GGUF"), vec![0_u8; 30]).unwrap();
        std::fs::write(models.join("notes.txt"), "x").unwrap();
        std::fs::create_dir_all(home.path().join("Downloads/nested")).unwrap();
        std::fs::write(home.path().join("Downloads/nested/deep.gguf"), "x").unwrap();

        let found = find(home.path());
        assert_eq!(
            found
                .models
                .iter()
                .map(|model| (model.path.ends_with("big.GGUF"), model.bytes))
                .collect::<Vec<_>>(),
            [(true, 30), (false, 10)],
            "only .gguf files, top level only, largest first"
        );
        assert!(
            found
                .looked_in
                .iter()
                .any(|folder| folder.ends_with("models"))
        );
    }

    #[test]
    fn an_empty_home_finds_nothing_and_says_where_it_looked() {
        let home = tempfile::tempdir().unwrap();
        let found = find(home.path());
        assert!(found.models.is_empty());
        assert!(found.looked_in.is_empty());
    }
}
