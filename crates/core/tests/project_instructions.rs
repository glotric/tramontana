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
use tramontana_core::config::{ConfigurationSelection, RawSource};
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
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        let stream_id = request.request().request_id.clone();
        self.requests.lock().unwrap().push(request);
        self.entered.notify_one();
        if self.hold.load(Ordering::SeqCst) {
            return Err(held_until_cancelled(&cancel).await);
        }
        Ok(answer_stream(stream_id, "Read and understood."))
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
    let service = test_service(
        Arc::new(InMemoryEventStore::new()),
        vec![Arc::clone(model) as Arc<dyn ModelProvider>],
        vec![Arc::new(FakeToolProvider::new())],
        Arc::new(ScriptedApprover::unavailable()),
        Arc::new(ReferenceWorker::new(
            vec!["fake.tool.echo".into()],
            ReadOnlyLimits::default(),
        )),
    )
    .with_routing_mode(RoutingMode::LocalOnly);
    match files {
        Some(files) => service.with_project_sources(files),
        None => service,
    }
}

/// A conversation in a new project rooted at `/fixture`.
async fn conversation(service: &TaskService) -> ConversationId {
    let project = service
        .create_project("Fixture", vec!["/fixture".into()])
        .await
        .unwrap();
    service
        .create_conversation(Some(project.project_id), "Question")
        .await
        .unwrap()
        .conversation_id
}

/// Start a task in a project and wait for it to end.
async fn run(service: &TaskService, project: bool) -> Vec<EventEnvelope> {
    let conversation_id = if project {
        Some(conversation(service).await)
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
    wait_events(
        service,
        &task,
        Duration::from_secs(20),
        Duration::from_millis(20),
        ended,
    )
    .await
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

    let conversation = conversation(&service).await;
    let task = service
        .start_task(start_request(
            &conversation,
            "Say what the project asks for.",
        ))
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

    let conversation = conversation(&service).await;
    let task = service
        .start_task(start_request(
            &conversation,
            "Say what the project asks for.",
        ))
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

    wait_state(
        &service,
        &task,
        Duration::from_secs(20),
        Duration::from_millis(20),
        TaskState::is_terminal,
    )
    .await;

    let messages = model.system_messages();
    assert!(messages[1].contains("Answer in one sentence."));
}

/// Start a task that names `selection`, and wait for it to end.
async fn run_with(
    service: &TaskService,
    selection: ConfigurationSelection,
) -> (TaskId, Vec<EventEnvelope>) {
    let conversation = conversation(service).await;
    let task = service
        .start_task(StartTaskRequest {
            selection: Some(selection),
            ..start_request(&conversation, "Say what the project asks for.")
        })
        .await
        .unwrap();
    let events = wait_events(
        service,
        &task,
        Duration::from_secs(20),
        Duration::from_millis(20),
        |events| {
            events
                .last()
                .is_some_and(|event| matches!(event.kind, TaskEventKind::TaskCompleted))
        },
    )
    .await;
    (task, events)
}

fn review_skill() -> RawSource {
    file(
        ".tramontana/skills/review.md",
        ConfigurationKind::Skill,
        "---\nid: review\nname: Review Rust\ndescription: Look for unsafe code.\n---\nRead every changed Rust file for unsafe blocks.",
    )
}

#[tokio::test]
async fn a_task_is_given_the_skills_it_names() {
    let model = Recording::new();
    let service = service(&model, Some(Files::holding(vec![review_skill()])));

    let (_, events) = run_with(
        &service,
        ConfigurationSelection {
            skill_ids: vec!["review".to_owned()],
            ..ConfigurationSelection::default()
        },
    )
    .await;

    let messages = model.system_messages();
    assert!(
        messages.len() > 1
            && messages[1].contains("Read every changed Rust file for unsafe blocks."),
        "the skill the person chose must reach the model; sent: {messages:?}"
    );
    assert!(source(&manifest(&events), ".tramontana/skills/review.md").included);
}

#[tokio::test]
async fn a_task_that_names_no_skill_is_given_none() {
    let model = Recording::new();
    let service = service(&model, Some(Files::holding(vec![review_skill()])));

    let (_, events) = run_with(&service, ConfigurationSelection::default()).await;

    assert_eq!(
        model.system_messages().len(),
        1,
        "offering a skill is not selecting one"
    );
    assert_eq!(
        source(&manifest(&events), ".tramontana/skills/review.md")
            .exclusion_reason
            .as_deref(),
        Some("context.notActive")
    );
}

#[tokio::test]
async fn a_task_that_named_a_skill_still_resumes() {
    // Continuing re-reads the files, and has to recover the choices the task
    // ran under: resolving with none of them would make a task that named a
    // skill look changed and refuse a resume nothing had touched (ADR 0053).
    let model = Recording::holding();
    let service = service(&model, Some(Files::holding(vec![review_skill()])));

    let conversation = conversation(&service).await;
    let task = service
        .start_task(StartTaskRequest {
            selection: Some(ConfigurationSelection {
                skill_ids: vec!["review".to_owned()],
                ..ConfigurationSelection::default()
            }),
            ..start_request(&conversation, "Say what the project asks for.")
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
    wait_state(
        &service,
        &task,
        Duration::from_secs(20),
        Duration::from_millis(20),
        TaskState::is_terminal,
    )
    .await;

    assert!(
        model
            .system_messages()
            .get(1)
            .is_some_and(|sent| sent.contains("Read every changed Rust file for unsafe blocks.")),
        "the continued run reads the same skill"
    );
}
