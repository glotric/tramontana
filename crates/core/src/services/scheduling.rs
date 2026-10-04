//! Running schedules while the application is open (ADR 0063).

use super::task_service::OrEnvelope as _;
use super::task_service::{SchedulePorts, StartTaskRequest, TaskService};
use crate::contracts::{
    ErrorCode, ErrorEnvelope, NewSchedule, SCHEMA_VERSION, Schedule, ScheduleView, TaskId,
};

impl TaskService {
    fn schedule_ports(&self) -> Result<&SchedulePorts, ErrorEnvelope> {
        self.schedules.as_ref().ok_or_else(|| {
            self.error(
                ErrorCode::CapabilityUnsupported,
                "This application keeps no schedules",
            )
        })
    }

    async fn find_schedule(&self, schedule_id: &str) -> Result<Schedule, ErrorEnvelope> {
        let (store, _) = self.schedule_ports()?;
        store
            .list()
            .await
            .or_envelope(self)?
            .into_iter()
            .find(|schedule| schedule.schedule_id == schedule_id)
            .ok_or_else(|| self.error(ErrorCode::NotFound, "No schedule has that id"))
    }

    async fn save_schedule(&self, schedule: &Schedule) -> Result<(), ErrorEnvelope> {
        let (store, _) = self.schedule_ports()?;
        store.save(schedule).await.or_envelope(self)
    }

    /// Every schedule, oldest first, with when each runs next.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::CapabilityUnsupported`] when the service keeps no
    /// schedules, or a persistence error.
    pub async fn list_schedules(&self) -> Result<Vec<ScheduleView>, ErrorEnvelope> {
        let (store, clock) = self.schedule_ports()?;
        let now = clock.local_now();
        Ok(store
            .list()
            .await
            .or_envelope(self)?
            .into_iter()
            .map(|schedule| ScheduleView {
                next_run: schedule
                    .enabled
                    .then(|| super::next_occurrence(schedule.cadence, &now)),
                schedule,
            })
            .collect())
    }

    /// Create a schedule and the conversation its runs are turns in.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for a schedule that cannot be kept,
    /// [`ErrorCode::NotFound`] for an unknown project, or as
    /// [`Self::list_schedules`].
    pub async fn create_schedule(&self, new: &NewSchedule) -> Result<ScheduleView, ErrorEnvelope> {
        let (store, clock) = self.schedule_ports()?;
        let (title, goal) = super::check_new_schedule(new)?;
        let existing = store.list().await.or_envelope(self)?;
        if existing.len() >= super::SCHEDULES {
            return Err(self.error(
                ErrorCode::InvalidRequest,
                format!(
                    "Tramontana keeps at most {} scheduled tasks",
                    super::SCHEDULES
                ),
            ));
        }
        let conversation = self
            .create_conversation(new.project_id.clone(), title.clone())
            .await?;
        let now = clock.local_now();
        let schedule = Schedule {
            schema_version: SCHEMA_VERSION,
            schedule_id: self.ids.next("schedule"),
            title,
            goal,
            project_id: new.project_id.clone(),
            conversation_id: conversation.conversation_id,
            cadence: new.cadence,
            routing_mode: new.routing_mode,
            enabled: true,
            created_at: self.clock.now(),
            created_local: now,
            last_run: None,
            last_task_id: None,
        };
        self.save_schedule(&schedule).await?;
        Ok(ScheduleView {
            next_run: Some(super::next_occurrence(schedule.cadence, &now)),
            schedule,
        })
    }

    /// Turn a schedule on or off. Turning one on does not run occurrences
    /// that passed while it was off.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`], or as [`Self::list_schedules`].
    pub async fn set_schedule_enabled(
        &self,
        schedule_id: &str,
        enabled: bool,
    ) -> Result<Schedule, ErrorEnvelope> {
        let (_, clock) = self.schedule_ports()?;
        let mut schedule = self.find_schedule(schedule_id).await?;
        if enabled && !schedule.enabled {
            // What passed while it was off is not owed.
            schedule.last_run = Some(super::latest_occurrence(
                schedule.cadence,
                &clock.local_now(),
            ));
        }
        schedule.enabled = enabled;
        self.save_schedule(&schedule).await?;
        Ok(schedule)
    }

    /// Delete a schedule. Its conversation, and every run in it, is kept.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::NotFound`], or as [`Self::list_schedules`].
    pub async fn delete_schedule(&self, schedule_id: &str) -> Result<(), ErrorEnvelope> {
        let (store, _) = self.schedule_ports()?;
        store.delete(schedule_id).await.or_envelope(self)
    }

    /// Run a schedule now, whatever its cadence says.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Conflict`] while its previous run is still under way,
    /// [`ErrorCode::NotFound`], or whatever starting the task reports.
    pub async fn run_schedule_now(&self, schedule_id: &str) -> Result<TaskId, ErrorEnvelope> {
        let schedule = self.find_schedule(schedule_id).await?;
        if self.previous_run_open(&schedule).await {
            return Err(self.error(
                ErrorCode::Conflict,
                "This schedule's previous run has not finished",
            ));
        }
        let task_id = self.start_scheduled(&schedule).await?;
        let mut schedule = schedule;
        schedule.last_task_id = Some(task_id.clone());
        self.save_schedule(&schedule).await?;
        Ok(task_id)
    }

    /// Start every schedule that is due now, once each (ADR 0063), and
    /// return the tasks started. A schedule whose previous run is still
    /// under way or waiting is passed for this occurrence rather than run
    /// twice. One schedule failing to start does not stop the others.
    ///
    /// # Errors
    ///
    /// As [`Self::list_schedules`].
    pub async fn run_due_schedules(&self) -> Result<Vec<TaskId>, ErrorEnvelope> {
        let (store, clock) = self.schedule_ports()?;
        let now = clock.local_now();
        let schedules = store.list().await.or_envelope(self)?;
        let mut started = Vec::new();
        for mut schedule in schedules {
            let Some(occurrence) = super::due_occurrence(&schedule, &now) else {
                continue;
            };
            schedule.last_run = Some(occurrence);
            if !self.previous_run_open(&schedule).await
                && let Ok(task_id) = self.start_scheduled(&schedule).await
            {
                schedule.last_task_id = Some(task_id.clone());
                started.push(task_id);
            }
            // Recorded either way: an occurrence is run or passed, never
            // retried on every check.
            self.save_schedule(&schedule).await?;
        }
        Ok(started)
    }

    /// Whether the schedule's last run has not reached a final state.
    async fn previous_run_open(&self, schedule: &Schedule) -> bool {
        let Some(task_id) = &schedule.last_task_id else {
            return false;
        };
        self.get_task_record(task_id)
            .await
            .is_ok_and(|task| !task.state.is_terminal())
    }

    /// Start one run: an ordinary task in the schedule's conversation, with
    /// the service's default worker and nothing more (ADR 0063).
    async fn start_scheduled(&self, schedule: &Schedule) -> Result<TaskId, ErrorEnvelope> {
        self.start_task(StartTaskRequest {
            conversation_id: Some(schedule.conversation_id.clone()),
            goal: schedule.goal.clone(),
            routing_mode: Some(schedule.routing_mode),
            ..StartTaskRequest::default()
        })
        .await
    }
}
