use std::path::{Path, PathBuf};

use tramontana_core::contracts::{AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope};
use tramontana_platform::{CanonicalRoot, canonicalize_root, contains};

/// Resolve a caller-supplied path against the authority the request carries.
///
/// This is the single containment check every filesystem tool goes through.
/// It exists as its own function so that adding a tool cannot accidentally add
/// a way around it.
///
/// Three things are enforced, in order:
///
/// 1. the request must carry at least one root, so a projectless task reaches
///    nothing;
/// 2. the path is resolved through its nearest existing ancestor, which
///    defeats a symlinked parent;
/// 3. the resolved path must lie inside a granted root — not merely look like
///    it does.
///
/// # Errors
///
/// [`ErrorCode::PermissionDenied`] when the path is outside every granted
/// root, and [`ErrorCode::NotFound`] when it cannot be resolved at all.
pub(crate) fn resolve_within_grant(
    request: &AuthorizedToolRequest,
    relative: &str,
) -> Result<PathBuf, ErrorEnvelope> {
    let correlation = CorrelationId::new(request.invocation_id().as_str());

    let deny = |message: String| {
        ErrorEnvelope::new(ErrorCode::PermissionDenied, message, correlation.clone())
    };

    if request.granted_scope().roots.is_empty() {
        return Err(deny(
            "this task has no approved project roots, so it can read nothing".to_owned(),
        ));
    }

    // An absolute path from a caller is not automatically wrong — it may name
    // a file inside a granted root — but it is never *trusted*. It goes
    // through the same containment check as a relative one.
    let candidate = Path::new(relative);

    // On Windows a path can be inside the root by every string test and
    // still not name a file there: `CON` or `nul.txt` opens a device, and
    // `notes.txt:hidden` a stream no listing shows. Containment is about
    // what a path reaches, so those are refused before it is judged.
    if cfg!(windows) && reaches_past_the_file_system(candidate) {
        return Err(deny(format!(
            "path names a Windows device or alternate stream: {}",
            display_safely(relative)
        )));
    }

    for granted in &request.granted_scope().roots {
        let Ok(root) = canonicalize_root(Path::new(granted)) else {
            // A root that no longer resolves grants nothing. Failing closed
            // here matters: a project folder can be renamed or unmounted while
            // a task is running.
            continue;
        };

        let full = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.as_path().join(candidate)
        };

        if contains(&root, &full).unwrap_or(false) {
            return Ok(full);
        }
    }

    Err(deny(format!(
        "path is outside every approved root: {}",
        display_safely(relative)
    )))
}

/// Whether a path names something other than a file or folder on Windows.
///
/// Any component that is a reserved device name — `CON`, `PRN`, `AUX`, `NUL`,
/// `COM0`–`COM9`, `LPT0`–`LPT9`, and their superscript-digit forms — whatever
/// its case, extension, or trailing dots and spaces; or that carries a colon,
/// which outside the drive prefix selects an alternate data stream.
///
/// Pure and platform-independent, so it can be tested everywhere; it is only
/// enforced on Windows, where these names mean something.
fn reaches_past_the_file_system(path: &Path) -> bool {
    path.components().any(|component| {
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy();
        name.contains(':') || is_reserved_device_name(&name)
    })
}

fn is_reserved_device_name(name: &str) -> bool {
    // Windows ignores everything from the first dot, and trailing spaces.
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    // `COM` or `LPT` and exactly one digit, superscripts included.
    let mut number = match upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    {
        Some(number) => number.chars(),
        None => return false,
    };
    let digit = |c: char| c.is_ascii_digit() || matches!(c, '\u{b9}' | '\u{b2}' | '\u{b3}');
    number.next().is_some_and(digit) && number.next().is_none()
}

/// Whether one granted root contains both paths.
///
/// Each path must already have passed [`resolve_within_grant`]; this adds that
/// they are inside the *same* root, so an operation on the pair cannot carry
/// anything from one approved folder into another.
pub(crate) fn same_root(request: &AuthorizedToolRequest, a: &Path, b: &Path) -> bool {
    request.granted_scope().roots.iter().any(|granted| {
        canonicalize_root(Path::new(granted)).is_ok_and(|root| {
            contains(&root, a).unwrap_or(false) && contains(&root, b).unwrap_or(false)
        })
    })
}

/// Express a path relative to the granted root, for display.
///
/// Durable events and approval prompts must not carry absolute paths: one
/// names a machine and usually a person (`PROJECT_FORMAT.md` §3).
#[must_use]
pub(crate) fn relative_to_grant(request: &AuthorizedToolRequest, path: &Path) -> String {
    for granted in &request.granted_scope().roots {
        if let Ok(root) = canonicalize_root(Path::new(granted))
            && let Ok(relative) = path.strip_prefix(root.as_path())
        {
            let portable = to_portable(relative);
            // The root itself strips to an empty string. Rendering that as
            // nothing produces "Listed 2 entries in " — say what was meant.
            return if portable.is_empty() {
                ".".to_owned()
            } else {
                portable
            };
        }
    }
    // Never fall back to the absolute path.
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<path>".to_owned())
}

/// Render a relative path with `/` separators on every platform.
fn to_portable(path: &Path) -> String {
    let mut text = String::new();
    for component in path.components() {
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(&component.as_os_str().to_string_lossy());
    }
    text
}

/// Trim a caller-supplied string for an error message.
///
/// The path is echoed so a person can see what was refused, but bounded so a
/// pathological argument cannot flood the log.
fn display_safely(value: &str) -> String {
    const MAX: usize = 120;
    if value.chars().count() <= MAX {
        return value.to_owned();
    }
    let truncated: String = value.chars().take(MAX).collect();
    format!("{truncated}…")
}

/// The first granted root, resolved.
///
/// # Errors
///
/// [`ErrorCode::PermissionDenied`] when the request carries no usable root.
pub(crate) fn primary_root(
    request: &AuthorizedToolRequest,
) -> Result<CanonicalRoot, ErrorEnvelope> {
    request
        .granted_scope()
        .roots
        .iter()
        .find_map(|root| canonicalize_root(Path::new(root)).ok())
        .ok_or_else(|| {
            ErrorEnvelope::new(
                ErrorCode::PermissionDenied,
                "this task has no approved project roots".to_owned(),
                CorrelationId::new(request.invocation_id().as_str()),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::reaches_past_the_file_system;

    #[test]
    fn a_windows_device_name_is_recognised_however_it_is_spelled() {
        for path in [
            "CON",
            "con",
            "nul.txt",
            "src/aux.c",
            "Prn.tar.gz",
            "COM1",
            "lpt9.log",
            "com\u{b9}",
            "NUL ",
            "NUL.",
            "CONIN$",
            "deep/nested/conout$.txt",
        ] {
            assert!(reaches_past_the_file_system(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn an_alternate_data_stream_is_recognised() {
        assert!(reaches_past_the_file_system(Path::new("notes.txt:hidden")));
        assert!(reaches_past_the_file_system(Path::new(
            "src/main.rs::$DATA"
        )));
    }

    #[test]
    fn ordinary_names_that_only_resemble_a_device_are_left_alone() {
        for path in [
            "console.log",
            "icon.png",
            "nullable.rs",
            "auxiliary/notes.md",
            "COM10",
            "LPT",
            "COMA",
            "src/com1x.rs",
            "greeting.txt",
        ] {
            assert!(!reaches_past_the_file_system(Path::new(path)), "{path}");
        }
    }
}
