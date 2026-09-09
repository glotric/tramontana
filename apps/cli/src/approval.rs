//! Terminal approval prompt.
//!
//! `BUILD_ORDER.md` §11 requires an adapter supporting allow once, allow for
//! this project where eligible, and deny. This is presentation only: which
//! answers are offered comes from the core, and an answer that was never
//! offered is refused rather than honored.

use std::io::{BufRead, IsTerminal, Write};

use async_trait::async_trait;
use tramontana_core::contracts::{ApprovalDecisionKind, ApprovalRequest};
use tramontana_core::ports::{Approver, PermissionError};

/// Asks on the terminal.
#[derive(Debug, Default)]
pub(crate) struct TerminalApprover;

impl TerminalApprover {
    /// An approver that prompts on standard input.
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Approver for TerminalApprover {
    async fn ask(
        &self,
        request: &ApprovalRequest,
    ) -> Result<ApprovalDecisionKind, PermissionError> {
        // A pipe or a CI job has no one to ask. Reading from a redirected
        // stdin would silently take whatever happened to be there as consent.
        if !std::io::stdin().is_terminal() {
            return Err(PermissionError::NoApprover);
        }

        let offered: Vec<(char, ApprovalDecisionKind, &str)> = request
            .eligible_decisions
            .iter()
            .map(|decision| match decision {
                ApprovalDecisionKind::AllowOnce => ('o', *decision, "allow once"),
                ApprovalDecisionKind::AllowProject => ('p', *decision, "allow for this project"),
                ApprovalDecisionKind::Deny => ('d', *decision, "deny"),
            })
            .collect();

        // Rendered to stderr so a piped `--json` stream stays machine-readable.
        let mut out = std::io::stderr();
        let _ = writeln!(out, "\n  Approval required");
        let _ = writeln!(out, "  {}", request.summary);
        for detail in &request.details {
            let _ = writeln!(out, "    {}: {}", detail.label, detail.value);
        }
        let choices: Vec<String> = offered
            .iter()
            .map(|(key, _, label)| format!("[{key}] {label}"))
            .collect();
        let _ = write!(out, "  {} > ", choices.join("  "));
        let _ = out.flush();

        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer).is_err() {
            return Err(PermissionError::NoApprover);
        }

        let chosen = answer.trim().chars().next().map(|c| c.to_ascii_lowercase());
        let decision = offered
            .iter()
            .find(|(key, _, _)| Some(*key) == chosen)
            .map(|(_, decision, _)| *decision)
            // Anything unrecognized — including an empty line — is a refusal.
            // Silence is not consent, and defaulting to allow would make a
            // stray keystroke an authorization.
            .unwrap_or(ApprovalDecisionKind::Deny);

        let _ = writeln!(out, "  -> {decision:?}\n");
        Ok(decision)
    }
}
