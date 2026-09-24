//! What a project asks for reaches the worker's model, and the record says so.
//!
//! ADR 0052. The files were always discovered, ordered and hashed; these
//! tests are about the step that was missing — their text arriving in front
//! of a model, and the manifest describing what actually went.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use async_trait::async_trait;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    sync::{Notify, mpsc},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;
use tramontana_core::config::RawSource;
use tramontana_core::{contracts::*, ports::*, services::*, testing::*};

/// A model that answers at once and keeps every request it was given.
struct Recording {
    requests: Mutex<Vec<AuthorizedModelRequest>>,
    /// Stay in the call, so the task can be checkpointed while it runs.
    hold: AtomicBool,
    entered: Notify,
}

impl Recording {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            hold: AtomicBool::new(false),
            entered: Notify::new(),
        })
    }

    fn holding() -> Arc<Self> {
        let model = Self::new();
        model.hold.store(true, Ordering::SeqCst);
        model
    }

    /// The system messages of the first turn, in the order they were sent.
    fn system_messages(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap();
        let first = requests.first().expect("the model was never called");
        first
            .request()
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::System)
            .map(|message| {
                message
                    .parts
                    .iter()
                    .map(|part| match part {
                        MessagePart::Text { text } => text.clone(),
                        _ => String::new(),
                    })
                    .collect::<String>()
            })
            .collect()
    }
}

#[async_trait]
impl ModelProvider for Recording {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake")
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        let mut descriptor = FakeModelProvider::descriptor();
        descriptor.capabilities.push(ModelCapability::ToolCalls);
        vec![descriptor]
    }

    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        let stream_id = request.request().request_id.clone();
        self.requests.lock().unwrap().push(request);
        self.entered.notify_one();
        if self.hold.load(Ordering::SeqCst) {
            _cancel.cancelled().await;
            return Err(ErrorEnvelope::new(
                ErrorCode::Cancelled,
                "cancelled",
                CorrelationId::new("test"),
            ));
        }
        let (sender, receiver) = mpsc::channel(4);
        sender
            .send(ModelEvent::TextDelta {
                stream_id,
                offset: 0,
                text: "Read and understood.".into(),
            })
            .await
            .unwrap();
        sender
            .send(ModelEvent::Completed {
                stop_reason: StopReason::Stop,
            })
            .await
            .unwrap();
        Ok(receiver)
    }
}

/// The project's files, as an adapter would hand them over.
struct Files {
    sources: Mutex<Vec<RawSource>>,
}

impl Files {
    fn holding(sources: Vec<RawSource>) -> Arc<Self> {
        Arc::new(Self {
            sources: Mutex::new(sources),
        })
    }
}

#[async_trait]
impl ProjectSourceReader for Files {
    async fn read(&self, _project_id: &ProjectId) -> Result<Vec<RawSource>, ErrorEnvelope> {
        Ok(self.sources.lock().unwrap().clone())
    }
}

fn file(path: &str, kind: ConfigurationKind, content: &str) -> RawSource {
    RawSource {
        relative_path: path.to_owned(),
        kind,
        content: content.as_bytes().to_vec(),
    }
}

fn service(model: &Arc<Recording>, files: Option<Arc<Files>>) -> TaskService {
    let events = Arc::new(InMemoryEventStore::new());
    let ids = Arc::new(SequentialIdSource::new());
    let (tools, permissions, releases) = assemble_tool_plane(
        vec![Arc::new(FakeToolProvider::new()) as Arc<dyn ToolProvider>],
        Arc::new(InMemoryApprovalStore::new()),
        Arc::new(ScriptedApprover::unavailable()),
        ids.clone(),
    )
    .unwrap();
    let service = TaskService::new(
        Repositories {
            projects: Arc::new(InMemoryProjectRepository::new()),
            conversations: Arc::new(InMemoryConversationRepository::new()),
            tasks: Arc::new(InMemoryTaskRepository::with_events(events.clone())),
            events,
            configuration: Arc::new(InMemoryConfigurationRepository::new()),
            checkpoints: Arc::new(InMemoryCheckpointRepository::new()),
        },
        Providers {
            models: vec![Arc::clone(model) as Arc<dyn ModelProvider>],
            tools,
            permissions,
            releases,
            worker: Arc::new(ReferenceWorker::new(
                vec!["fake.tool.echo".into()],
                ReadOnlyLimits::default(),
            )),
        },
        Arc::new(FakeClock::new()),
        ids,
    )
    .with_routing_mode(RoutingMode::LocalOnly);
    match files {
        Some(files) => service.with_project_sources(files),
        None => service,
    }
}

/// Start a task in a project and wait for it to end.
async fn run(service: &TaskService, project: bool) -> Vec<EventEnvelope> {
    let conversation_id = if project {
        let project = service
            .create_project("Fixture", vec!["/fixture".into()])
            .await
            .unwrap();
        Some(
            service
                .create_conversation(Some(project.project_id), "Question")
                .await
                .unwrap()
                .conversation_id,
        )
    } else {
        None
    };
    let task = service
        .start_task(StartTaskRequest {
            conversation_id,
            goal: "Say what the project asks for.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(20), async {
        loop {
            let events = service.replay_task_events(&task, 0).await.unwrap();
            if events.last().is_some_and(|event| {
                matches!(
                    event.kind,
                    TaskEventKind::TaskCompleted
                        | TaskEventKind::TaskFailed
                        | TaskEventKind::TaskCancelled
                )
            }) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

/// The context manifest the task recorded.
fn manifest(events: &[EventEnvelope]) -> ContextManifest {
    events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ContextPrepared { manifest } => Some(manifest.clone()),
            _ => None,
        })
        .expect("a task records the context it prepared")
}

fn source<'a>(manifest: &'a ContextManifest, source_id: &str) -> &'a ContextSource {
    manifest
        .sources
        .iter()
        .find(|source| source.source_id == source_id)
        .unwrap_or_else(|| panic!("{source_id} is not in the manifest"))
}

#[tokio::test]
async fn the_model_reads_the_project_s_instructions_after_tramontana_s_own() {
    let model = Recording::new();
    let service = service(
        &model,
        Some(Files::holding(vec![
            file(
                "AGENTS.md",
                ConfigurationKind::RootAgents,
                "Always run the project's tests before answering.",
            ),
            file(
                ".tramontana/rules/style.md",
                ConfigurationKind::Rule,
                "---\nid: style\n---\nBritish spelling, everywhere.",
            ),
        ])),
    );

    let events = run(&service, true).await;
    assert!(
        events
            .last()
            .is_some_and(|event| event.kind == TaskEventKind::TaskCompleted)
    );

    let messages = model.system_messages();
    assert_eq!(messages.len(), 2, "Tramontana's words, then the project's");
    assert_eq!(
        messages[0], WORKER_INSTRUCTIONS,
        "how to work is read before what this project wants"
    );
    assert!(messages[1].contains("Always run the project's tests before answering."));
    assert!(messages[1].contains("British spelling, everywhere."));
    assert!(messages[1].contains("AGENTS.md"));
    assert!(
        messages[1].find("style.md").unwrap() < messages[1].find("AGENTS.md").unwrap(),
        "precedence decides what is read first: a rule outranks the root AGENTS.md \
         (`CONFIGURATION.md` §4)"
    );
    assert!(
        !messages[1].contains("id: style"),
        "front matter is metadata, not an instruction"
    );
}

#[tokio::test]
async fn the_record_names_what_travelled_and_what_did_not() {
    let model = Recording::new();
    let service = service(
        &model,
        Some(Files::holding(vec![
            file(
                "AGENTS.md",
                ConfigurationKind::RootAgents,
                "Prefer small changes.",
            ),
            // A skill nobody selected is in the inventory and out of the task.
            file(
                ".tramontana/skills/review.md",
                ConfigurationKind::Skill,
                "---\nid: review\n---\nReview Rust for unsafe code.",
            ),
        ])),
    );

    let events = run(&service, true).await;
    let manifest = manifest(&events);

    let agents = source(&manifest, "AGENTS.md");
    assert!(agents.included, "an active file's text was sent");
    assert_eq!(agents.exclusion_reason, None);

    let skill = source(&manifest, ".tramontana/skills/review.md");
    assert!(!skill.included);
    assert_eq!(
        skill.exclusion_reason.as_deref(),
        Some("context.notActive"),
        "a skill nobody selected is gathered and deliberately not used"
    );
    assert!(
        !model.system_messages()[1].contains("Review Rust for unsafe code."),
        "an inventory is not a decision to send"
    );
}

#[tokio::test]
async fn an_instruction_file_larger_than_the_turn_is_left_out_whole_and_named() {
    let model = Recording::new();
    // Over the 16 KiB a turn carries, and under the 1 MiB a file may be.
    let long = "Follow this rule to the letter. ".repeat(1_000);
    let service = service(
        &model,
        Some(Files::holding(vec![
            file("AGENTS.md", ConfigurationKind::RootAgents, &long),
            file(
                "CLAUDE.md",
                ConfigurationKind::Claude,
                "Short, and still read.",
            ),
        ])),
    );

    let events = run(&service, true).await;

    let sent = &model.system_messages()[1];
    assert!(sent.contains("Short, and still read."));
    assert!(
        !sent.contains(&"Follow this rule to the letter. ".repeat(20)),
        "half a rule is worse than none"
    );
    assert!(
        sent.contains("Left out") && sent.contains("AGENTS.md"),
        "the model must not answer as though it had read everything"
    );

    let manifest = manifest(&events);
    let agents = source(&manifest, "AGENTS.md");
    assert!(!agents.included);
    assert_eq!(
        agents.exclusion_reason.as_deref(),
        Some("context.overInstructionBudget")
    );
    assert!(source(&manifest, "CLAUDE.md").included);
}

#[tokio::test]
async fn a_credential_in_an_instruction_file_never_reaches_the_model() {
    let model = Recording::new();
    let service = service(
        &model,
        Some(Files::holding(vec![file(
            "AGENTS.md",
            ConfigurationKind::RootAgents,
            "Deploy with API_KEY=sk-live-0123456789abcdefghij and nothing else.",
        )])),
    );

    let events = run(&service, true).await;

    let sent = &model.system_messages()[1];
    assert!(
        !sent.contains("sk-live-0123456789abcdefghij"),
        "the text sent is the redacted text the configuration hashed"
    );
    assert!(sent.contains("Deploy with"));
    assert!(
        !source(&manifest(&events), "AGENTS.md")
            .redactions
            .is_empty(),
        "and the record says something was taken out of it"
    );
}

#[tokio::test]
async fn a_task_outside_a_project_is_sent_no_project_instructions() {
    let model = Recording::new();
    let service = service(
        &model,
        Some(Files::holding(vec![file(
            "AGENTS.md",
            ConfigurationKind::RootAgents,
            "This project's rule.",
        )])),
    );

    run(&service, false).await;

    let messages = model.system_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0], WORKER_INSTRUCTIONS);
}

#[tokio::test]
async fn a_service_that_cannot_read_the_project_s_files_runs_as_it_always_did() {
    let model = Recording::new();
    let service = service(&model, None);

    let events = run(&service, true).await;

    assert!(
        events
            .last()
            .is_some_and(|event| event.kind == TaskEventKind::TaskCompleted)
    );
    assert_eq!(model.system_messages().len(), 1);
}

#[tokio::test]
async fn an_instruction_file_that_changed_stops_a_resume() {
    // The refusal already existed, and compared the task's hash with the last
    // inventory *stored* for the project — which an edit on disk never
    // touched. It now reads the files (ADR 0052).
    let model = Recording::holding();
    let files = Files::holding(vec![file(
        "AGENTS.md",
        ConfigurationKind::RootAgents,
        "Answer in one sentence.",
    )]);
    let service = service(&model, Some(Arc::clone(&files)));

    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Question")
        .await
        .unwrap();
    let task = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Say what the project asks for.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(5), model.entered.notified())
        .await
        .unwrap();

    let checkpoint = service.create_checkpoint(&task).await.unwrap();
    assert_eq!(checkpoint.task_state, TaskState::Interrupted);

    // The same task, resumable, until someone edits the file it ran under.
    *files.sources.lock().unwrap() = vec![file(
        "AGENTS.md",
        ConfigurationKind::RootAgents,
        "Answer in one sentence, and cite the file.",
    )];

    let refusal = service
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap_err();

    assert_eq!(refusal.code, ErrorCode::Conflict);
    assert!(
        refusal.message.contains("Configuration changed"),
        "the model must never be given instructions the record does not describe"
    );
}

#[tokio::test]
async fn an_untouched_project_resumes_as_it_always_did() {
    // The other half of the refusal above: reading the files again must not
    // make every resume look like a change.
    let model = Recording::holding();
    let files = Files::holding(vec![file(
        "AGENTS.md",
        ConfigurationKind::RootAgents,
        "Answer in one sentence.",
    )]);
    let service = service(&model, Some(files));

    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    let conversation = service
        .create_conversation(Some(project.project_id), "Question")
        .await
        .unwrap();
    let task = service
        .start_task(StartTaskRequest {
            conversation_id: Some(conversation.conversation_id),
            goal: "Say what the project asks for.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(5), model.entered.notified())
        .await
        .unwrap();
    let checkpoint = service.create_checkpoint(&task).await.unwrap();

    model.hold.store(false, Ordering::SeqCst);
    service
        .resume_task(&task, &checkpoint.checkpoint_id)
        .await
        .unwrap();

    timeout(Duration::from_secs(20), async {
        loop {
            let state = service.get_task(&task).await.unwrap().state;
            if state.is_terminal() {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let messages = model.system_messages();
    assert!(messages[1].contains("Answer in one sentence."));
}
