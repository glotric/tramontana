//! Export, import, and handoff as application commands.
//!
//! The rules about what may leave the machine live in `services::export`,
//! which is pure. This is the part that reads storage and writes it back, so
//! the CLI and the desktop shell cannot each invent their own idea of what an
//! export contains.

use super::task_service::OrEnvelope as _;
use crate::contracts::{
    ErrorCode, ErrorEnvelope, ExportBundle, ExportOptions, HandoffBundle, ImportedRecords,
    ProjectId, TaskId,
};
use crate::services::export::{ExportRequest, ImportLimits, build_export, validate_import};
use crate::services::handoff::build_handoff;
use crate::services::task_service::TaskService;

/// What an import wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// Projects created, without their roots.
    pub projects: usize,
    /// Conversations created.
    pub conversations: usize,
    /// Tasks created.
    pub tasks: usize,
    /// Durable events restored.
    pub events: usize,
    /// Checkpoints restored.
    pub checkpoints: usize,
    /// Configuration manifests restored.
    pub configurations: usize,
    /// Opaque worker state that was refused rather than restored.
    pub quarantined: usize,
}

impl TaskService {
    /// Assemble an export bundle for one task.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task does not exist, or a persistence
    /// error when storage is unavailable.
    pub async fn export_task(
        &self,
        task_id: &TaskId,
        options: ExportOptions,
    ) -> Result<ExportBundle, ErrorEnvelope> {
        let record = self.get_task_record(task_id).await?;
        let mut request = ExportRequest::at(self.clock.now());
        request.options = options;

        if let Some(project_id) = &record.project_id {
            request.project = Some(self.get_project(project_id).await?);
            request.configurations = self
                .configurations_for(std::slice::from_ref(&record))
                .await?;
        }
        if let Some(conversation_id) = &record.conversation_id {
            request.conversations = vec![self.get_conversation(conversation_id).await?];
        }
        request.events = self.replay_task_events(task_id, 0).await?;
        request.checkpoints = self.checkpoints_for(task_id).await?;
        request.tasks = vec![record];

        Ok(build_export(request))
    }

    /// Assemble an export bundle for a whole project, or for projectless work.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the project does not exist, or a
    /// persistence error when storage is unavailable.
    pub async fn export_project(
        &self,
        project_id: Option<&ProjectId>,
        options: ExportOptions,
    ) -> Result<ExportBundle, ErrorEnvelope> {
        let mut request = ExportRequest::at(self.clock.now());
        request.options = options;

        if let Some(project_id) = project_id {
            request.project = Some(self.get_project(project_id).await?);
        }
        request.conversations = self.list_conversations(project_id).await?;

        // Every task in those conversations, plus the projectless ones when
        // exporting projectless work. Listing by conversation keeps a task
        // from another project out of a project's bundle.
        let wanted: Vec<_> = request
            .conversations
            .iter()
            .map(|conversation| conversation.conversation_id.clone())
            .collect();
        for task in self.list_tasks(None).await? {
            let belongs = match (&task.conversation_id, project_id) {
                (Some(conversation_id), _) => wanted.contains(conversation_id),
                (None, None) => true,
                (None, Some(_)) => false,
            };
            if !belongs {
                continue;
            }
            request
                .events
                .extend(self.replay_task_events(&task.task_id, 0).await?);
            request
                .checkpoints
                .extend(self.checkpoints_for(&task.task_id).await?);
            request.tasks.push(task);
        }

        request.configurations = self.configurations_for(&request.tasks).await?;
        Ok(build_export(request))
    }

    /// Validate a bundle without writing anything.
    ///
    /// The staging step. A caller shows the user what a bundle contains and
    /// what it omits before anything touches the profile.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] describing the first rule the bundle
    /// broke.
    pub fn stage_import(
        &self,
        bundle: &ExportBundle,
        limits: ImportLimits,
    ) -> Result<ImportedRecords, ErrorEnvelope> {
        validate_import(bundle, limits).map_err(|error| {
            // The rejection reason is safe to show: it names a rule and a
            // bundle-relative path, never the content that broke it.
            self.error(ErrorCode::InvalidRequest, error.to_string())
        })
    }

    /// Validate a bundle and write its records into this profile.
    ///
    /// Nothing in a bundle is executed. Imported tasks are records: resuming
    /// one is a separate, explicit command that recomputes eligibility from
    /// the events that were just written.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] when the bundle is refused,
    /// [`ErrorCode::Conflict`] when a record already exists, or a persistence
    /// error when storage is unavailable.
    pub async fn import_bundle(
        &self,
        bundle: &ExportBundle,
        limits: ImportLimits,
    ) -> Result<ImportSummary, ErrorEnvelope> {
        let records = self.stage_import(bundle, limits)?;
        let mut summary = ImportSummary {
            quarantined: records.quarantined.len(),
            ..ImportSummary::default()
        };

        for project in &records.projects {
            self.repositories
                .projects
                .create(project)
                .await
                .or_envelope(self)?;
            summary.projects += 1;
        }
        // Before the tasks: a task records which manifest it started with, and
        // a task pointing at a manifest that is not there could not say what
        // instructions were in force.
        for entry in &records.configurations {
            self.repositories
                .configuration
                .save(&entry.project_id, &entry.configuration)
                .await
                .or_envelope(self)?;
            summary.configurations += 1;
        }
        for conversation in &records.conversations {
            self.repositories
                .conversations
                .create(conversation)
                .await
                .or_envelope(self)?;
            summary.conversations += 1;
        }

        for task in &records.tasks {
            self.repositories
                .tasks
                .create(task)
                .await
                .or_envelope(self)?;
            summary.tasks += 1;

            if let Some(events) = records.events.get(task.task_id.as_str()) {
                // Appended against an empty stream: an imported task is new
                // here, and appending onto an existing one would interleave
                // two histories into an order neither intended.
                self.repositories
                    .events
                    .append(&task.task_id, 0, events.clone())
                    .await
                    .map_err(|error| self.error(ErrorCode::PersistenceError, error.to_string()))?;
                summary.events += events.len();
            }
        }

        for checkpoint in &records.checkpoints {
            self.repositories
                .checkpoints
                .save(checkpoint)
                .await
                .or_envelope(self)?;
            summary.checkpoints += 1;
        }

        // The task rows carry a cached state and sequence from the machine
        // that exported them. Rebuilding from the events just written is what
        // makes the imported record self-consistent here.
        self.repositories
            .tasks
            .rebuild_from_events()
            .await
            .or_envelope(self)?;

        Ok(summary)
    }

    /// Assemble everything a different worker would need to continue a task.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`] when the task has no checkpoint to hand over.
    pub async fn handoff_task(&self, task_id: &TaskId) -> Result<HandoffBundle, ErrorEnvelope> {
        let record = self.get_task_record(task_id).await?;
        let checkpoint = self.latest_checkpoint(task_id).await?.ok_or_else(|| {
            self.error(
                ErrorCode::NotFound,
                "this task has no checkpoint; create one before handing it over",
            )
        })?;
        let events = self.replay_task_events(task_id, 0).await?;

        Ok(build_handoff(
            &record,
            &events,
            &checkpoint,
            self.clock.now(),
        ))
    }

    /// The configuration manifest each task started with, deduplicated.
    ///
    /// A task records a manifest hash; the bundle has to carry the manifest
    /// itself, or an imported task would reference something nothing can
    /// resolve.
    async fn configurations_for(
        &self,
        tasks: &[crate::contracts::TaskRecord],
    ) -> Result<Vec<crate::contracts::ExportedConfiguration>, ErrorEnvelope> {
        let mut wanted: Vec<(ProjectId, String)> = Vec::new();
        for task in tasks {
            if let (Some(project_id), Some(hash)) =
                (&task.project_id, &task.configuration_manifest_hash)
            {
                let pair = (project_id.clone(), hash.clone());
                if !wanted.contains(&pair) {
                    wanted.push(pair);
                }
            }
        }

        let mut found = Vec::new();
        for (project_id, hash) in wanted {
            // A manifest the repository no longer holds is not an export
            // failure: the task still records the hash it ran under, and the
            // omission is visible as a manifest the bundle cannot resolve.
            if let Ok(configuration) = self
                .repositories
                .configuration
                .get(&project_id, &hash)
                .await
            {
                found.push(crate::contracts::ExportedConfiguration {
                    project_id,
                    configuration,
                });
            }
        }
        Ok(found)
    }

    /// Every checkpoint recorded for a task, oldest first.
    async fn checkpoints_for(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<crate::contracts::TaskCheckpoint>, ErrorEnvelope> {
        self.repositories
            .checkpoints
            .list(task_id)
            .await
            .or_envelope(self)
    }
}
