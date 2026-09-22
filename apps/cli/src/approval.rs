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
        let stdin = std::io::stdin();
        // Rendered to stderr so a piped `--json` stream stays machine-readable.
        put_question(request, &stdin, &mut stdin.lock(), &mut std::io::stderr())
    }
}

/// Puts `request` to whoever is at `terminal`: shows it on `out` and takes
/// the answer from the next line of `input`.
///
/// In use these are this process's own stdin and stderr. They are parameters
/// so the tests can put the question to a pseudo-terminal and check the whole
/// exchange rather than its parts.
fn put_question(
    request: &ApprovalRequest,
    terminal: &impl Terminal,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<ApprovalDecisionKind, PermissionError> {
    // A pipe or a CI job has no one to ask. Reading from a redirected
    // stdin would silently take whatever happened to be there as consent.
    if !terminal.is_interactive() {
        return Err(PermissionError::NoApprover);
    }

    // Anything typed before the question appeared was not an answer to it:
    // "o" and Enter pressed while a task was streaming output would otherwise
    // be read as consent to whatever is asked next. If that input cannot be
    // discarded, the next line cannot be trusted either.
    if terminal.discard_typeahead().is_err() {
        return Err(PermissionError::NoApprover);
    }

    let offered: Vec<(char, ApprovalDecisionKind, &str)> = request
        .eligible_decisions
        .iter()
        .map(|decision| match decision {
            ApprovalDecisionKind::AllowOnce => ('o', *decision, "allow once"),
            ApprovalDecisionKind::AllowProject => ('p', *decision, "allow for this project"),
            ApprovalDecisionKind::AllowSite => ('s', *decision, "allow this site for this task"),
            ApprovalDecisionKind::Deny => ('d', *decision, "deny"),
        })
        .collect();

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
    if input.read_line(&mut answer).is_err() {
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

/// The terminal a question is put to.
trait Terminal {
    /// Whether a person is at it, rather than a pipe or a file.
    fn is_interactive(&self) -> bool;

    /// Throws away input that reached it before the prompt did.
    ///
    /// Whatever is typed after this returns is kept, so it must be called
    /// before the prompt is shown, not after.
    fn discard_typeahead(&self) -> std::io::Result<()>;
}

#[cfg(unix)]
impl<T: std::os::fd::AsFd> Terminal for T {
    fn is_interactive(&self) -> bool {
        self.as_fd().is_terminal()
    }

    /// Empties the kernel's queue of received-but-unread input (`tcflush`
    /// with `TCIFLUSH`).
    fn discard_typeahead(&self) -> std::io::Result<()> {
        rustix::termios::tcflush(self, rustix::termios::QueueSelector::IFlush)?;
        Ok(())
    }
}

#[cfg(not(unix))]
impl Terminal for std::io::Stdin {
    fn is_interactive(&self) -> bool {
        self.is_terminal()
    }

    /// Not done on Windows yet, where typeahead still reaches the prompt.
    fn discard_typeahead(&self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, reason = "assertions in tests")]
mod tests {
    use std::fs::File;
    use std::io::{BufReader, Seek, Write};

    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use tramontana_core::contracts::{
        ApprovalDecisionKind, ApprovalId, ApprovalRequest, Capability, CapabilityScope, RiskClass,
        SideEffect, TaskId,
    };
    use tramontana_core::ports::PermissionError;

    use super::{Terminal, put_question};

    /// A pseudo-terminal. What is written to `keyboard` arrives at `terminal`
    /// exactly as keystrokes arrive at the terminal the prompt reads from.
    struct Pty {
        keyboard: File,
        terminal: File,
    }

    fn pty() -> Pty {
        let controller =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open a pseudo-terminal");
        grantpt(&controller).expect("grant the pseudo-terminal");
        unlockpt(&controller).expect("unlock the pseudo-terminal");
        let path = ptsname(&controller, Vec::new()).expect("name the pseudo-terminal");
        let terminal = rustix::fs::open(
            path.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        )
        .expect("open the terminal side");
        Pty {
            keyboard: File::from(controller),
            terminal: File::from(terminal),
        }
    }

    /// Stands in for the person at the terminal. They see what is written to
    /// the screen and, once the prompt has been flushed to it, type `answer`.
    struct Person<'a> {
        keyboard: &'a File,
        answer: &'a [u8],
        screen: Vec<u8>,
    }

    impl Write for Person<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.screen.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut keyboard = self.keyboard;
            keyboard.write_all(std::mem::take(&mut self.answer))
        }
    }

    /// A terminal that keeps typeahead, as the prompt used to.
    struct KeepsTypeahead<'a>(&'a File);

    impl Terminal for KeepsTypeahead<'_> {
        fn is_interactive(&self) -> bool {
            self.0.is_interactive()
        }

        fn discard_typeahead(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A terminal whose pending input cannot be discarded.
    struct CannotDiscard;

    impl Terminal for CannotDiscard {
        fn is_interactive(&self) -> bool {
            true
        }

        fn discard_typeahead(&self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }

    /// A question offering all three answers. The tests answer "p", which is
    /// neither the "o" typed ahead nor the fallback to deny, so a result of
    /// `AllowProject` can only have come from what was typed at the prompt.
    fn request() -> ApprovalRequest {
        ApprovalRequest {
            schema_version: tramontana_core::SCHEMA_VERSION,
            approval_id: ApprovalId::new("approval-1"),
            task_id: TaskId::new("task-1"),
            invocation_id: None,
            capability: Capability::new("fs.write"),
            tool_id: "write_file".to_owned(),
            requested_scope: CapabilityScope {
                project_id: None,
                roots: Vec::new(),
                releases_data: false,
            },
            risk_class: RiskClass::Modify,
            side_effect: SideEffect::LocalReversible,
            summary: "write_file wants to fs.write".to_owned(),
            details: Vec::new(),
            eligible_decisions: vec![
                ApprovalDecisionKind::AllowOnce,
                ApprovalDecisionKind::AllowProject,
                ApprovalDecisionKind::Deny,
            ],
        }
    }

    #[test]
    fn typeahead_is_taken_as_the_answer_when_it_is_kept() {
        // The failure being guarded against, through the real prompt: "o"
        // and Enter typed before the question are read as its answer. It
        // also shows the pseudo-terminal really queues input, so the next
        // test cannot pass merely because nothing was queued.
        let mut pty = pty();
        pty.keyboard.write_all(b"o\n").expect("type ahead");
        let mut person = Person {
            keyboard: &pty.keyboard,
            answer: b"p\n",
            screen: Vec::new(),
        };

        let decision = put_question(
            &request(),
            &KeepsTypeahead(&pty.terminal),
            &mut BufReader::new(&pty.terminal),
            &mut person,
        )
        .expect("an answer");

        assert_eq!(decision, ApprovalDecisionKind::AllowOnce);
    }

    #[test]
    fn typeahead_is_not_taken_as_the_answer() {
        let mut pty = pty();
        // "o" and Enter, pressed while a task was streaming output.
        pty.keyboard.write_all(b"o\n").expect("type ahead");
        let mut person = Person {
            keyboard: &pty.keyboard,
            answer: b"p\n",
            screen: Vec::new(),
        };

        let decision = put_question(
            &request(),
            &pty.terminal,
            &mut BufReader::new(&pty.terminal),
            &mut person,
        )
        .expect("an answer");

        assert_eq!(decision, ApprovalDecisionKind::AllowProject);
        let screen = String::from_utf8_lossy(&person.screen);
        assert!(screen.contains("Approval required"), "{screen}");
        assert!(screen.contains("-> AllowProject"), "{screen}");
    }

    #[test]
    fn a_site_can_be_allowed_for_the_task_only_where_offered() {
        let site = || ApprovalRequest {
            eligible_decisions: vec![
                ApprovalDecisionKind::AllowOnce,
                ApprovalDecisionKind::AllowSite,
                ApprovalDecisionKind::Deny,
            ],
            ..request()
        };
        for (question, typed, expected) in [
            (site(), &b"s\n"[..], ApprovalDecisionKind::AllowSite),
            // Not offered, so "s" is not an answer, and anything else is no.
            (request(), &b"s\n"[..], ApprovalDecisionKind::Deny),
        ] {
            let pty = pty();
            let mut person = Person {
                keyboard: &pty.keyboard,
                answer: typed,
                screen: Vec::new(),
            };
            let decision = put_question(
                &question,
                &pty.terminal,
                &mut BufReader::new(&pty.terminal),
                &mut person,
            )
            .expect("an answer");
            assert_eq!(decision, expected);
            let screen = String::from_utf8_lossy(&person.screen);
            assert_eq!(
                screen.contains("[s] allow this site for this task"),
                question
                    .eligible_decisions
                    .contains(&ApprovalDecisionKind::AllowSite),
                "{screen}"
            );
        }
    }

    #[test]
    fn nothing_is_asked_when_input_is_not_a_terminal() {
        // Stdin redirected from a file that happens to say "o" is not
        // consent: nothing is shown and nothing is read.
        let mut file = tempfile::tempfile().expect("create a file");
        file.write_all(b"o\n").expect("write the file");
        file.rewind().expect("rewind the file");
        let mut screen = Vec::new();

        let decision = put_question(&request(), &file, &mut BufReader::new(&file), &mut screen);

        assert!(
            matches!(decision, Err(PermissionError::NoApprover)),
            "{decision:?}"
        );
        assert!(screen.is_empty());
    }

    #[test]
    fn nothing_is_asked_when_typeahead_cannot_be_discarded() {
        let mut screen = Vec::new();

        let decision = put_question(&request(), &CannotDiscard, &mut &b"o\n"[..], &mut screen);

        assert!(
            matches!(decision, Err(PermissionError::NoApprover)),
            "{decision:?}"
        );
        assert!(screen.is_empty());
    }

    #[test]
    fn a_failure_to_discard_is_reported() {
        // The refusal above only protects anyone if a real failure surfaces
        // instead of being swallowed.
        let file = tempfile::tempfile().expect("create a file");
        assert!(file.discard_typeahead().is_err());
    }
}
