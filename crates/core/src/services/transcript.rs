//! A readable copy of one conversation, to share as a file (ADR 0060).
//!
//! Rendered here so the window and the terminal write the same file. It holds
//! what a person would call the conversation: what was asked, which files
//! were attached, and what came back. The record keeps everything else.

use crate::contracts::{
    ConversationRecord, EventEnvelope, EventPayload, TaskAttachment, TaskRecord, TranscriptFormat,
    TranscriptOptions,
};

/// One turn, as the transcript shows it.
struct Turn<'a> {
    goal: &'a str,
    attachments: &'a [TaskAttachment],
    outcome: Outcome,
    tool_steps: usize,
}

enum Outcome {
    Answered(String),
    Failed(String),
    Stopped,
    Unfinished,
}

fn turn<'a>(record: &'a TaskRecord, events: &'a [EventEnvelope]) -> Turn<'a> {
    let attachments = events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::TaskCreated { attachments, .. } => Some(attachments.as_slice()),
            _ => None,
        })
        .unwrap_or_default();
    let tool_steps = events
        .iter()
        .filter(|event| matches!(event.payload, EventPayload::ToolRequested { .. }))
        .count();
    let answer = events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::MessageCommitted { message }
            if message.role == crate::contracts::MessageRole::Assistant
                && message.status == crate::contracts::MessageStatus::Committed =>
        {
            let text: Vec<&str> = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    crate::contracts::MessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let text = text.join("\n");
            (!text.trim().is_empty()).then(|| super::redact(text.trim()).text)
        }
        _ => None,
    });
    let ended = events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::TaskFailed { error } => Some(Outcome::Failed(error.message.clone())),
        EventPayload::TaskCancelled { .. } => Some(Outcome::Stopped),
        EventPayload::TaskCompleted { .. } => Some(Outcome::Unfinished),
        _ => None,
    });
    let outcome = match (answer, ended) {
        (_, Some(Outcome::Failed(message))) => Outcome::Failed(message),
        (Some(answer), _) => Outcome::Answered(answer),
        (None, Some(Outcome::Stopped)) => Outcome::Stopped,
        _ => Outcome::Unfinished,
    };
    Turn {
        goal: &record.goal_summary,
        attachments,
        outcome,
        tool_steps,
    }
}

/// A Markdown code fence that nothing in `text` can close early.
fn fence_for(text: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for character in text.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn steps_note(steps: usize) -> Option<String> {
    match steps {
        0 => None,
        1 => Some("(1 tool step, not included)".to_owned()),
        _ => Some(format!("({steps} tool steps, not included)")),
    }
}

fn outcome_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Answered(text) => text.clone(),
        Outcome::Failed(message) => format!("This answer failed: {message}"),
        Outcome::Stopped => "Stopped before an answer.".to_owned(),
        Outcome::Unfinished => "No answer was recorded.".to_owned(),
    }
}

/// Render `conversation` and its tasks, oldest first, each with its events.
#[must_use]
pub fn render_transcript(
    conversation: &ConversationRecord,
    project_name: Option<&str>,
    tasks: &[(TaskRecord, Vec<EventEnvelope>)],
    options: &TranscriptOptions,
) -> String {
    let title = super::redact(&conversation.title).text;
    let started = conversation.created_at.as_str();
    let context = project_name.map_or_else(
        || "A chat in Tramontana".to_owned(),
        |name| format!("A conversation in the Tramontana project {name}"),
    );
    let turns: Vec<Turn<'_>> = tasks
        .iter()
        .map(|(record, events)| turn(record, events))
        .collect();
    match options.format {
        TranscriptFormat::Markdown => {
            let mut out = format!("# {title}\n\n{context}, started {started}.\n");
            for turn in &turns {
                out.push_str(&format!("\n## You\n\n{}\n", turn.goal));
                for attachment in turn.attachments {
                    out.push_str(&format!("\nAttached: {}\n", attachment.name));
                    if options.include_attachments {
                        let fence = fence_for(&attachment.text);
                        out.push_str(&format!("\n{fence}\n{}\n{fence}\n", attachment.text));
                    }
                }
                out.push_str(&format!(
                    "\n## Tramontana\n\n{}\n",
                    outcome_text(&turn.outcome)
                ));
                if let Some(note) = steps_note(turn.tool_steps) {
                    out.push_str(&format!("\n_{note}_\n"));
                }
            }
            out
        }
        TranscriptFormat::Html => {
            let paragraphs = |text: &str| -> String {
                text.split("\n\n")
                    .map(|paragraph| {
                        format!("<p>{}</p>", escape_html(paragraph).replace('\n', "<br>"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let mut body = String::new();
            for turn in &turns {
                body.push_str("<section class=\"you\"><h2>You</h2>\n");
                body.push_str(&paragraphs(turn.goal));
                for attachment in turn.attachments {
                    body.push_str(&format!(
                        "\n<p class=\"file\">Attached: {}</p>",
                        escape_html(&attachment.name)
                    ));
                    if options.include_attachments {
                        body.push_str(&format!("\n<pre>{}</pre>", escape_html(&attachment.text)));
                    }
                }
                body.push_str("</section>\n<section class=\"answer\"><h2>Tramontana</h2>\n");
                body.push_str(&paragraphs(&outcome_text(&turn.outcome)));
                if let Some(note) = steps_note(turn.tool_steps) {
                    body.push_str(&format!("\n<p class=\"note\">{}</p>", escape_html(&note)));
                }
                body.push_str("</section>\n");
            }
            format!(
                "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{title}</title>\n<style>\n\
body{{font:16px/1.6 -apple-system,BlinkMacSystemFont,\"Segoe UI\",Roboto,sans-serif;\
max-width:760px;margin:40px auto;padding:0 16px;color:#1b1e25;background:#f3f5f8}}\n\
h1{{font-size:28px;letter-spacing:-0.02em}}h2{{font-size:13px;color:#5b6170;margin:0 0 4px}}\n\
section{{margin:18px 0}}.you{{background:#e3e8f0;border-radius:18px;padding:12px 18px}}\n\
.file,.note{{color:#5b6170;font-size:14px}}pre{{white-space:pre-wrap;background:#fff;\
border:1px solid #dde1e8;border-radius:10px;padding:12px}}\n</style>\n</head>\n<body>\n\
<h1>{title}</h1>\n<p class=\"note\">{context}, started {started}.</p>\n{body}</body>\n</html>\n",
                title = escape_html(&title),
                context = escape_html(&context),
                started = escape_html(started),
            )
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{
        ConversationId, CorrelationId, EventId, MessageId, MessageRole, NormalizedMessage,
        SCHEMA_VERSION, TaskEventKind, TaskId, Timestamp, Visibility, content_hash,
    };

    fn envelope(task: &TaskId, sequence: u64, payload: EventPayload) -> EventEnvelope {
        EventEnvelope {
            schema_version: SCHEMA_VERSION,
            event_id: EventId::new(format!("{task}-{sequence}")),
            task_id: task.clone(),
            conversation_id: Some(ConversationId::new("c1")),
            sequence,
            occurred_at: Timestamp::new("2026-09-28T10:00:00.000Z"),
            kind: TaskEventKind::TaskCreated,
            visibility: Visibility::User,
            correlation_id: CorrelationId::new("c"),
            causation_id: None,
            payload,
        }
    }

    fn answered(
        id: &str,
        goal: &str,
        answer: &str,
        file: Option<&str>,
    ) -> (TaskRecord, Vec<EventEnvelope>) {
        let task = TaskId::new(id);
        let record = TaskRecord::new(
            task.clone(),
            Some(ConversationId::new("c1")),
            None,
            goal,
            Timestamp::new("2026-09-28T10:00:00.000Z"),
        );
        let attachments = file.map_or_else(Vec::new, |text| {
            vec![TaskAttachment {
                name: "notes.md".into(),
                media_type: "text/markdown".into(),
                text: text.into(),
                content_hash: content_hash(text),
                byte_count: text.len() as u64,
                redactions: Vec::new(),
            }]
        });
        let mut message = NormalizedMessage::text(
            MessageId::new(format!("{id}-a")),
            Some(ConversationId::new("c1")),
            MessageRole::Assistant,
            Timestamp::new("2026-09-28T10:00:01.000Z"),
            answer,
        );
        message.task_id = Some(task.clone());
        let events = vec![
            envelope(
                &task,
                1,
                EventPayload::TaskCreated {
                    goal_summary: goal.into(),
                    conversation_id: None,
                    routing_mode: None,
                    attachments,
                },
            ),
            envelope(&task, 2, EventPayload::MessageCommitted { message }),
        ];
        (record, events)
    }

    fn conversation() -> ConversationRecord {
        ConversationRecord::new(
            ConversationId::new("c1"),
            None,
            "Winds <of> the Adriatic",
            Timestamp::new("2026-09-28T10:00:00.000Z"),
        )
    }

    fn options(format: TranscriptFormat, include_attachments: bool) -> TranscriptOptions {
        TranscriptOptions {
            format,
            include_attachments,
        }
    }

    #[test]
    fn markdown_holds_the_questions_and_answers_in_order() {
        let tasks = [
            answered("t1", "Name a wind.", "Bora.", None),
            answered("t2", "Another?", "Jugo.", Some("```\nx\n```")),
        ];
        let text = render_transcript(
            &conversation(),
            None,
            &tasks,
            &options(TranscriptFormat::Markdown, false),
        );
        assert!(text.starts_with("# Winds <of> the Adriatic\n\nA chat in Tramontana"));
        let first = text.find("Name a wind.").unwrap();
        let second = text.find("Jugo.").unwrap();
        assert!(first < text.find("Bora.").unwrap() && text.find("Bora.").unwrap() < second);
        assert!(text.contains("Attached: notes.md"));
        assert!(
            !text.contains("```\nx"),
            "a file's text is left out unless asked for"
        );

        let with_files = render_transcript(
            &conversation(),
            None,
            &tasks,
            &options(TranscriptFormat::Markdown, true),
        );
        assert!(
            with_files.contains("````\n```\nx\n```\n````"),
            "the fence outlasts the file's own"
        );
    }

    #[test]
    fn html_escapes_everything_recorded_and_loads_nothing() {
        let tasks = [answered(
            "t1",
            "<script>alert(1)</script>",
            "Use <b>bold</b> & \"quotes\".",
            None,
        )];
        let html = render_transcript(
            &conversation(),
            Some("Harbour cranes"),
            &tasks,
            &options(TranscriptFormat::Html, false),
        );
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("Use &lt;b&gt;bold&lt;/b&gt; &amp; &quot;quotes&quot;."));
        assert!(html.contains("<title>Winds &lt;of&gt; the Adriatic</title>"));
        assert!(html.contains("the Tramontana project Harbour cranes"));
        assert!(!html.contains("src=") && !html.contains("href="));
    }

    #[test]
    fn an_answer_is_redacted_again_and_a_missing_one_says_why() {
        let key = format!("sk-proj-{}", "a".repeat(40));
        let tasks = [answered("t1", "Key?", &format!("It is {key}"), None)];
        let text = render_transcript(
            &conversation(),
            None,
            &tasks,
            &options(TranscriptFormat::Markdown, false),
        );
        assert!(!text.contains(&key));

        let task = TaskId::new("t2");
        let record = TaskRecord::new(
            task.clone(),
            Some(ConversationId::new("c1")),
            None,
            "Hi?",
            Timestamp::new("2026-09-28T10:00:00.000Z"),
        );
        let stopped = vec![envelope(
            &task,
            1,
            EventPayload::TaskCancelled {
                origin: crate::contracts::CancellationOrigin::User,
            },
        )];
        let text = render_transcript(
            &conversation(),
            None,
            &[(record, stopped)],
            &options(TranscriptFormat::Markdown, false),
        );
        assert!(text.contains("Stopped before an answer."));
    }
}
