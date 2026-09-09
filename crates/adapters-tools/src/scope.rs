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
