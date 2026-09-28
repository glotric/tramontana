//! The earlier turns of a conversation, as the next task's model sees them
//! (ADR 0057).
//!
//! A turn is an earlier task's goal and its final answer, read from the
//! canonical record and nothing else: a worker's private transcript never
//! becomes the next turn's context.

use crate::contracts::{
    ContextCategory, ContextSource, DataClass, EventEnvelope, EventPayload, MessagePart,
    MessageRole, MessageStatus, TaskId, TaskRecord, TaskState,
};

/// At most this many earlier turns are sent, newest kept first.
pub const EARLIER_TURNS: usize = 20;

/// At most this many bytes of earlier goals and answers are sent.
pub const EARLIER_TURN_BYTES: usize = 24 * 1024;

/// One earlier exchange: what was asked, and what came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EarlierTurn {
    /// The task it was.
    pub task_id: TaskId,
    /// Its goal, as recorded (already redacted at ingestion).
    pub goal: String,
    /// Its final answer, redacted again.
    pub answer: String,
    /// What was removed from the answer.
    pub answer_redactions: Vec<String>,
}

/// The turns a task is given, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EarlierTurns {
    turns: Vec<EarlierTurn>,
}

impl EarlierTurns {
    /// Choose the turns for `task` from the tasks of its conversation, each
    /// with its own events.
    ///
    /// Only tasks created before `task` that completed with an answer are
    /// eligible, so choosing again on resume gives the same turns. Kept
    /// newest first while they fit [`EARLIER_TURNS`] and
    /// [`EARLIER_TURN_BYTES`]; a turn that does not fit is left out whole,
    /// and so is every turn older than it.
    #[must_use]
    pub fn choose(task: &TaskRecord, earlier: &[(TaskRecord, Vec<EventEnvelope>)]) -> Self {
        let mut eligible: Vec<_> = earlier
            .iter()
            .filter(|(record, _)| {
                record.task_id != task.task_id
                    && record.conversation_id.is_some()
                    && record.conversation_id == task.conversation_id
                    && record.state == TaskState::Completed
                    && (record.created_at.as_str(), record.task_id.as_str())
                        < (task.created_at.as_str(), task.task_id.as_str())
            })
            .filter_map(|(record, events)| {
                let answer = final_answer(record, events)?;
                let redacted = super::redact(&answer);
                Some((
                    record,
                    EarlierTurn {
                        task_id: record.task_id.clone(),
                        goal: record.goal_summary.clone(),
                        answer: redacted.text,
                        answer_redactions: redacted.reason_codes,
                    },
                ))
            })
            .collect();
        eligible.sort_by(|(a, _), (b, _)| {
            (a.created_at.as_str(), a.task_id.as_str())
                .cmp(&(b.created_at.as_str(), b.task_id.as_str()))
        });

        let mut kept = Vec::new();
        let mut used = 0usize;
        for (_, turn) in eligible.into_iter().rev() {
            let size = turn.goal.len().saturating_add(turn.answer.len());
            if kept.len() == EARLIER_TURNS || used.saturating_add(size) > EARLIER_TURN_BYTES {
                break;
            }
            used = used.saturating_add(size);
            kept.push(turn);
        }
        kept.reverse();
        Self { turns: kept }
    }

    /// The turns, oldest first.
    #[must_use]
    pub fn turns(&self) -> &[EarlierTurn] {
        &self.turns
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Their context sources, oldest first, goal before answer.
    #[must_use]
    pub fn sources(&self) -> Vec<ContextSource> {
        self.turns
            .iter()
            .enumerate()
            .flat_map(|(index, turn)| {
                [
                    ContextSource::redacted(
                        goal_source_id(index),
                        ContextCategory::Conversation,
                        DataClass::Project,
                        &turn.goal,
                        Vec::new(),
                    ),
                    ContextSource::redacted(
                        answer_source_id(index),
                        ContextCategory::Conversation,
                        DataClass::Sensitive,
                        &turn.answer,
                        turn.answer_redactions.clone(),
                    ),
                ]
            })
            .collect()
    }
}

/// The source id of the `index`th turn's goal, oldest first.
#[must_use]
pub fn goal_source_id(index: usize) -> String {
    format!("conversation.turn.{index}.goal")
}

/// The source id of the `index`th turn's answer, oldest first.
#[must_use]
pub fn answer_source_id(index: usize) -> String {
    format!("conversation.turn.{index}.answer")
}

/// The text of a task's last committed assistant message, when it is text.
fn final_answer(record: &TaskRecord, events: &[EventEnvelope]) -> Option<String> {
    events.iter().rev().find_map(|event| match &event.payload {
        EventPayload::MessageCommitted { message }
            if message.role == MessageRole::Assistant
                && message.status == MessageStatus::Committed
                && message.task_id.as_ref() == Some(&record.task_id) =>
        {
            let text: Vec<&str> = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    MessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let text = text.join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{
        ConversationId, CorrelationId, MessageId, NormalizedMessage, SCHEMA_VERSION, TaskEventKind,
        Timestamp, Visibility,
    };

    fn task(id: &str, at: &str, state: TaskState, goal: &str) -> TaskRecord {
        let mut record = TaskRecord::new(
            TaskId::new(id),
            Some(ConversationId::new("c1")),
            None,
            goal,
            Timestamp::new(at),
        );
        record.state = state;
        record
    }

    fn answered(record: &TaskRecord, text: &str) -> Vec<EventEnvelope> {
        let mut message = NormalizedMessage::text(
            MessageId::new(format!("m-{}", record.task_id)),
            record.conversation_id.clone(),
            MessageRole::Assistant,
            Timestamp::new("2026-09-28T00:00:00.000Z"),
            text,
        );
        message.task_id = Some(record.task_id.clone());
        vec![EventEnvelope {
            schema_version: SCHEMA_VERSION,
            event_id: crate::contracts::EventId::new("e1"),
            task_id: record.task_id.clone(),
            conversation_id: record.conversation_id.clone(),
            sequence: 1,
            occurred_at: Timestamp::new("2026-09-28T00:00:00.000Z"),
            kind: TaskEventKind::MessageCommitted,
            visibility: Visibility::User,
            correlation_id: CorrelationId::new("c"),
            causation_id: None,
            payload: EventPayload::MessageCommitted { message },
        }]
    }

    fn with(record: TaskRecord, text: &str) -> (TaskRecord, Vec<EventEnvelope>) {
        let events = answered(&record, text);
        (record, events)
    }

    #[test]
    fn only_earlier_completed_turns_with_an_answer_are_given_oldest_first() {
        let now = task(
            "t9",
            "2026-09-28T00:09:00.000Z",
            TaskState::Queued,
            "Third?",
        );
        let earlier = vec![
            with(
                task(
                    "t2",
                    "2026-09-28T00:02:00.000Z",
                    TaskState::Completed,
                    "Second?",
                ),
                "Two.",
            ),
            with(
                task(
                    "t1",
                    "2026-09-28T00:01:00.000Z",
                    TaskState::Completed,
                    "First?",
                ),
                "One.",
            ),
            with(
                task(
                    "t3",
                    "2026-09-28T00:03:00.000Z",
                    TaskState::Failed,
                    "Broken?",
                ),
                "Half an answer",
            ),
            (
                task(
                    "t4",
                    "2026-09-28T00:04:00.000Z",
                    TaskState::Completed,
                    "Silent?",
                ),
                Vec::new(),
            ),
            with(
                task(
                    "t10",
                    "2026-09-28T00:10:00.000Z",
                    TaskState::Completed,
                    "Later?",
                ),
                "After.",
            ),
        ];
        let turns = EarlierTurns::choose(&now, &earlier);
        let goals: Vec<_> = turns
            .turns()
            .iter()
            .map(|turn| turn.goal.as_str())
            .collect();
        assert_eq!(goals, ["First?", "Second?"]);
        assert_eq!(turns.turns()[1].answer, "Two.");
    }

    #[test]
    fn another_conversation_is_never_read() {
        let now = task("t9", "2026-09-28T00:09:00.000Z", TaskState::Queued, "Now?");
        let mut elsewhere = task(
            "t1",
            "2026-09-28T00:01:00.000Z",
            TaskState::Completed,
            "Other?",
        );
        elsewhere.conversation_id = Some(ConversationId::new("c2"));
        let turns = EarlierTurns::choose(&now, &[with(elsewhere, "Not yours.")]);
        assert!(turns.is_empty());
    }

    #[test]
    fn the_newest_turns_are_kept_and_older_ones_left_out_whole() {
        let now = task("t99", "2026-09-28T01:00:00.000Z", TaskState::Queued, "Now?");
        let earlier: Vec<_> = (0..30)
            .map(|index| {
                with(
                    task(
                        &format!("t{index:02}"),
                        &format!("2026-09-28T00:{index:02}:00.000Z"),
                        TaskState::Completed,
                        &format!("Question {index}?"),
                    ),
                    "Answer.",
                )
            })
            .collect();
        let turns = EarlierTurns::choose(&now, &earlier);
        assert_eq!(turns.turns().len(), EARLIER_TURNS);
        assert_eq!(turns.turns()[0].goal, "Question 10?");
        assert_eq!(turns.turns().last().unwrap().goal, "Question 29?");

        let long = "x".repeat(EARLIER_TURN_BYTES - 10);
        let big = vec![
            with(
                task(
                    "a",
                    "2026-09-28T00:01:00.000Z",
                    TaskState::Completed,
                    "Old?",
                ),
                "Old answer.",
            ),
            with(
                task(
                    "b",
                    "2026-09-28T00:02:00.000Z",
                    TaskState::Completed,
                    "Big?",
                ),
                &long,
            ),
        ];
        let turns = EarlierTurns::choose(&now, &big);
        assert_eq!(turns.turns().len(), 1);
        assert_eq!(turns.turns()[0].goal, "Big?");
    }

    #[test]
    fn an_answer_is_redacted_and_says_so_on_its_source() {
        let now = task("t9", "2026-09-28T00:09:00.000Z", TaskState::Queued, "Now?");
        let key = format!("sk-proj-{}", "a".repeat(40));
        let turns = EarlierTurns::choose(
            &now,
            &[with(
                task(
                    "t1",
                    "2026-09-28T00:01:00.000Z",
                    TaskState::Completed,
                    "Key?",
                ),
                &format!("It is {key}"),
            )],
        );
        assert!(!turns.turns()[0].answer.contains(&key));
        let sources = turns.sources();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_id, "conversation.turn.0.goal");
        assert_eq!(sources[0].data_class, DataClass::Project);
        assert_eq!(sources[1].data_class, DataClass::Sensitive);
        assert!(!sources[1].redactions.is_empty());
    }
}
