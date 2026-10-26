//! Leases on rented GPUs, and the money they spend (ADR 0072).
//!
//! The adapter knows one provider's API; this owns every decision that costs
//! money. Three rules are load-bearing:
//!
//! - **Fail closed on cost.** A lease whose cost cannot be read is not
//!   started, and a new lease is not started while any running lease's cost
//!   is unreadable.
//! - **Reserve before spending.** Each model call reserves its maximum cost
//!   (rate × deadline) against the cap and releases the unused part when it
//!   ends, under one lock, so concurrent calls cannot together overshoot.
//! - **Every lease stops.** On idle and on cap from here; on the provider's
//!   own idle limit when Tramontana is gone. Stop always terminates.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::contracts::{
    AuthorizedModelRequest, ComputeLease, ComputeLeaseEvent, ComputeSpec, CorrelationId, ErrorCode,
    ErrorEnvelope, LeaseId, LeaseStatus, LeaseStopReason, ModelDescriptor, ModelEvent,
    ProviderConfig, ProviderId, SCHEMA_VERSION, cost_of_millis,
};
use crate::ports::{Clock, ComputeLeaseJournal, ComputeProvisioner, ModelProvider};

/// The person's limits for rented compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeasePolicy {
    /// The most every lease together may cost, in micro-dollars.
    ///
    /// shortcut: one cap for the life of this service, not a calendar day;
    /// add a daily window when the desktop keeps the service across days.
    pub cap_micro_usd: u64,
    /// Stop a lease after this long with no call.
    pub idle_limit: Duration,
    /// How often a running lease's status and cost are read.
    pub check_every: Duration,
    /// Give up on a lease that is not serving after this long.
    pub ready_timeout: Duration,
}

/// A lease that is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyLease {
    /// The lease.
    pub lease_id: LeaseId,
    /// Its OpenAI-compatible endpoint, for `OpenAiCompatibleProvider`.
    pub endpoint: ProviderConfig,
    /// Workers kept warm with no requests. Above zero, it bills while idle
    /// and the interface must say so.
    pub always_on_workers: u32,
}

/// Starts, meters and stops leases at one provider.
#[derive(Clone)]
pub struct ComputeLeases {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ComputeLeases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputeLeases")
            .field("policy", &self.inner.policy)
            .finish_non_exhaustive()
    }
}

struct Inner {
    provisioner: Arc<dyn ComputeProvisioner>,
    journal: Arc<dyn ComputeLeaseJournal>,
    clock: Arc<dyn Clock>,
    policy: LeasePolicy,
    ledger: Mutex<Ledger>,
    closed: CancellationToken,
}

#[derive(Default)]
struct Ledger {
    running: HashMap<LeaseId, Running>,
    /// What stopped leases cost.
    stopped_spent: u64,
}

struct Running {
    rate: u64,
    /// The provider's figure, which lags.
    provider_spent: u64,
    /// Tramontana's own count of finished calls.
    settled: u64,
    reserved: u64,
    in_flight: u32,
    last_used: Instant,
    stopping: bool,
}

impl Running {
    fn spent(&self) -> u64 {
        self.provider_spent.max(self.settled)
    }
}

impl Ledger {
    /// Spent plus reserved, across every lease.
    fn committed(&self) -> u64 {
        self.running
            .values()
            .map(|lease| lease.spent().saturating_add(lease.reserved))
            .fold(self.stopped_spent, u64::saturating_add)
    }
}

fn refused(code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("compute-leases"))
}

impl ComputeLeases {
    /// A service over one provider.
    #[must_use]
    pub fn new(
        provisioner: Arc<dyn ComputeProvisioner>,
        journal: Arc<dyn ComputeLeaseJournal>,
        clock: Arc<dyn Clock>,
        policy: LeasePolicy,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                provisioner,
                journal,
                clock,
                policy,
                ledger: Mutex::new(Ledger::default()),
                closed: CancellationToken::new(),
            }),
        }
    }

    /// Start a lease and wait until it serves.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LimitExceeded`] at the cap, [`ErrorCode::ProviderUnavailable`]
    /// when any cost cannot be read, or the provider's error. A lease that
    /// was created and then failed is terminated before this returns.
    pub async fn start(&self, spec: &ComputeSpec) -> Result<ReadyLease, ErrorEnvelope> {
        let inner = &self.inner;
        if inner.closed.is_cancelled() {
            return Err(refused(ErrorCode::Cancelled, "Rented compute has closed"));
        }
        // Fail closed: every running lease's cost must be readable first.
        let running: Vec<LeaseId> = inner.ledger().running.keys().cloned().collect();
        for lease_id in running {
            let cost = inner.provisioner.cost(&lease_id).await.map_err(|_| {
                refused(
                    ErrorCode::ProviderUnavailable,
                    "A running lease's cost cannot be read, so no new lease is started",
                )
            })?;
            if let Some(lease) = inner.ledger().running.get_mut(&lease_id) {
                lease.provider_spent = lease.provider_spent.max(cost.spent_micro_usd);
            }
        }
        if inner.ledger().committed() >= inner.policy.cap_micro_usd {
            return Err(refused(
                ErrorCode::LimitExceeded,
                "The spending cap for rented compute is reached",
            ));
        }

        let lease = inner.provisioner.provision(spec).await?;
        if let Err(failure) = inner
            .journal
            .append(&ComputeLeaseEvent::started(
                &lease,
                &spec.model_id,
                inner.clock.now(),
            ))
            .await
        {
            let _ = inner.provisioner.stop(&lease.lease_id).await;
            return Err(failure);
        }
        inner.ledger().running.insert(
            lease.lease_id.clone(),
            Running {
                rate: lease.rate_micro_usd_per_hour,
                provider_spent: 0,
                settled: 0,
                reserved: 0,
                in_flight: 0,
                last_used: Instant::now(),
                stopping: false,
            },
        );

        match inner.until_ready(spec, &lease).await {
            Ok(ready) => {
                inner.spawn_watcher(lease.lease_id.clone());
                Ok(ready)
            }
            Err(failure) => {
                let _ = inner.stop(&lease.lease_id, LeaseStopReason::Failure).await;
                Err(failure)
            }
        }
    }

    /// Reserve one call's maximum cost on a lease.
    ///
    /// The reservation settles when dropped: what the call actually ran for
    /// is kept, the rest released.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::LimitExceeded`] when the call could overshoot the cap
    /// (an idle lease is then stopped), or [`ErrorCode::NotFound`] when the
    /// lease is not running.
    pub async fn reserve(
        &self,
        lease_id: &LeaseId,
        deadline: Duration,
    ) -> Result<Reservation, ErrorEnvelope> {
        let inner = &self.inner;
        let idle_at_cap = {
            let mut ledger = inner.ledger();
            let committed = ledger.committed();
            let lease = ledger
                .running
                .get_mut(lease_id)
                .filter(|lease| !lease.stopping)
                .ok_or_else(|| refused(ErrorCode::NotFound, "That lease is not running"))?;
            let maximum = cost_of_millis(lease.rate, millis(deadline));
            if committed.saturating_add(maximum) <= inner.policy.cap_micro_usd {
                lease.reserved = lease.reserved.saturating_add(maximum);
                lease.in_flight += 1;
                lease.last_used = Instant::now();
                return Ok(Reservation {
                    inner: Arc::clone(inner),
                    lease_id: lease_id.clone(),
                    maximum,
                    started: Instant::now(),
                });
            }
            lease.in_flight == 0
        };
        // Nothing in flight can free budget: the lease can serve no more.
        if idle_at_cap {
            let _ = inner.stop(lease_id, LeaseStopReason::Cap).await;
        }
        Err(refused(
            ErrorCode::LimitExceeded,
            "This call could pass the spending cap for rented compute",
        ))
    }

    /// The person stops a lease.
    ///
    /// # Errors
    ///
    /// The provider's error when it could not be told; the lease is kept and
    /// tried again on the next check.
    pub async fn stop(&self, lease_id: &LeaseId) -> Result<(), ErrorEnvelope> {
        self.inner.stop(lease_id, LeaseStopReason::Person).await
    }

    /// Tramontana is closing: stop everything it started.
    pub async fn close(&self) {
        self.inner.closed.cancel();
        let running: Vec<LeaseId> = self.inner.ledger().running.keys().cloned().collect();
        for lease_id in running {
            let _ = self.inner.stop(&lease_id, LeaseStopReason::AppClosed).await;
        }
    }

    /// Spent plus reserved, in micro-dollars, for a live meter.
    #[must_use]
    pub fn committed_micro_usd(&self) -> u64 {
        self.inner.ledger().committed()
    }

    /// Leases still running.
    #[must_use]
    pub fn running(&self) -> Vec<LeaseId> {
        self.inner.ledger().running.keys().cloned().collect()
    }
}

impl Inner {
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        // A poisoned ledger is still the best record of what is running.
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn until_ready(
        &self,
        spec: &ComputeSpec,
        lease: &ComputeLease,
    ) -> Result<ReadyLease, ErrorEnvelope> {
        if lease.rate_micro_usd_per_hour > spec.max_rate_micro_usd_per_hour {
            return Err(refused(
                ErrorCode::PolicyBlocked,
                "The provider's rate is above the highest rate accepted",
            ));
        }
        // Fail closed: a lease whose cost cannot be read does not serve.
        self.provisioner.cost(&lease.lease_id).await.map_err(|_| {
            refused(
                ErrorCode::ProviderUnavailable,
                "The new lease's cost cannot be read, so it was stopped",
            )
        })?;
        let give_up = Instant::now() + self.policy.ready_timeout;
        loop {
            match self.provisioner.status(&lease.lease_id).await? {
                LeaseStatus::Ready {
                    endpoint,
                    always_on_workers,
                } => {
                    if always_on_workers > 0 {
                        tracing::warn!(
                            lease = %lease.lease_id,
                            always_on_workers,
                            "a rented-compute endpoint keeps workers running while idle"
                        );
                    }
                    self.journal
                        .append(&ComputeLeaseEvent::ComputeLeaseReady {
                            schema_version: SCHEMA_VERSION,
                            lease_id: lease.lease_id.clone(),
                            rate_micro_usd_per_hour: lease.rate_micro_usd_per_hour,
                            always_on_workers,
                            occurred_at: self.clock.now(),
                        })
                        .await?;
                    if let Some(running) = self.ledger().running.get_mut(&lease.lease_id) {
                        running.last_used = Instant::now();
                    }
                    return Ok(ReadyLease {
                        lease_id: lease.lease_id.clone(),
                        endpoint,
                        always_on_workers,
                    });
                }
                LeaseStatus::Failed { reason } => {
                    return Err(refused(ErrorCode::ProviderUnavailable, reason));
                }
                LeaseStatus::Stopping | LeaseStatus::Stopped => {
                    return Err(refused(
                        ErrorCode::ProviderUnavailable,
                        "The provider stopped the lease before it served",
                    ));
                }
                _ => {}
            }
            if Instant::now() >= give_up {
                return Err(refused(
                    ErrorCode::Timeout,
                    "The lease did not start serving in time",
                ));
            }
            tokio::time::sleep(self.policy.check_every).await;
        }
    }

    fn spawn_watcher(self: &Arc<Self>, lease_id: LeaseId) {
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(inner.policy.check_every);
            loop {
                tokio::select! {
                    () = inner.closed.cancelled() => return,
                    _ = tick.tick() => {}
                }
                if !inner.ledger().running.contains_key(&lease_id) {
                    return;
                }
                inner.check(&lease_id).await;
            }
        });
    }

    /// One look at a running lease: gone, over the cap, or idle.
    async fn check(&self, lease_id: &LeaseId) {
        if matches!(
            self.provisioner.status(lease_id).await,
            Ok(LeaseStatus::Stopped | LeaseStatus::Failed { .. })
        ) {
            self.forget(lease_id, LeaseStopReason::Watchdog).await;
            return;
        }
        // An unreadable cost here does not stop the lease: the reservations
        // still bound it, and a new lease is refused until it reads again.
        if let Ok(cost) = self.provisioner.cost(lease_id).await
            && let Some(lease) = self.ledger().running.get_mut(lease_id)
        {
            lease.provider_spent = lease.provider_spent.max(cost.spent_micro_usd);
        }
        let reason = {
            let ledger = self.ledger();
            let Some(lease) = ledger.running.get(lease_id) else {
                return;
            };
            let spent = ledger
                .running
                .values()
                .map(Running::spent)
                .fold(ledger.stopped_spent, u64::saturating_add);
            if spent >= self.policy.cap_micro_usd {
                Some(LeaseStopReason::Cap)
            } else if lease.in_flight == 0 && lease.last_used.elapsed() >= self.policy.idle_limit {
                Some(LeaseStopReason::Idle)
            } else {
                None
            }
        };
        if let Some(reason) = reason {
            let _ = self.stop(lease_id, reason).await;
        }
    }

    async fn stop(&self, lease_id: &LeaseId, reason: LeaseStopReason) -> Result<(), ErrorEnvelope> {
        {
            let mut ledger = self.ledger();
            match ledger.running.get_mut(lease_id) {
                Some(lease) if !lease.stopping => lease.stopping = true,
                // Already gone, or someone else is stopping it.
                _ => return Ok(()),
            }
        }
        if let Err(failure) = self.provisioner.stop(lease_id).await {
            if let Some(lease) = self.ledger().running.get_mut(lease_id) {
                lease.stopping = false;
            }
            return Err(failure);
        }
        self.forget(lease_id, reason).await;
        Ok(())
    }

    /// Record a lease as stopped and carry its cost into the total.
    async fn forget(&self, lease_id: &LeaseId, reason: LeaseStopReason) {
        let Some((rate, spent)) = ({
            let mut ledger = self.ledger();
            ledger.running.remove(lease_id).map(|lease| {
                // Calls still in flight are counted at their maximum.
                let spent = lease
                    .spent()
                    .max(lease.settled.saturating_add(lease.reserved));
                ledger.stopped_spent = ledger.stopped_spent.saturating_add(spent);
                (lease.rate, spent)
            })
        }) else {
            return;
        };
        let event = ComputeLeaseEvent::ComputeLeaseStopped {
            schema_version: SCHEMA_VERSION,
            lease_id: lease_id.clone(),
            reason,
            rate_micro_usd_per_hour: rate,
            spent_micro_usd: spent,
            occurred_at: self.clock.now(),
        };
        if let Err(failure) = self.journal.append(&event).await {
            tracing::error!(lease = %lease_id, code = ?failure.code, "a lease stop was not recorded");
        }
    }
}

/// One call's hold on the budget. Dropping it settles the call.
#[derive(Debug)]
pub struct Reservation {
    inner: Arc<Inner>,
    lease_id: LeaseId,
    maximum: u64,
    started: Instant,
}

opaque_debug!(Inner);

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut ledger = self.inner.ledger();
        // A lease stopped mid-call already counted this call at its maximum.
        if let Some(lease) = ledger.running.get_mut(&self.lease_id) {
            let used = cost_of_millis(lease.rate, millis(self.started.elapsed())).min(self.maximum);
            lease.reserved = lease.reserved.saturating_sub(self.maximum);
            lease.settled = lease.settled.saturating_add(used);
            lease.in_flight = lease.in_flight.saturating_sub(1);
            lease.last_used = Instant::now();
        }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// A model on a lease: every call reserves its maximum cost first.
///
/// Wraps the provider that serves the lease's endpoint — in practice
/// `OpenAiCompatibleProvider` — so the budget is held for exactly as long as
/// the call's stream is open.
pub struct LeasedModel {
    leases: ComputeLeases,
    lease_id: LeaseId,
    model: Arc<dyn ModelProvider>,
}

impl std::fmt::Debug for LeasedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeasedModel")
            .field("lease_id", &self.lease_id)
            .finish_non_exhaustive()
    }
}

impl LeasedModel {
    /// Meter `model`'s calls against `lease_id`.
    #[must_use]
    pub fn new(leases: ComputeLeases, lease_id: LeaseId, model: Arc<dyn ModelProvider>) -> Self {
        Self {
            leases,
            lease_id,
            model,
        }
    }
}

#[async_trait]
impl ModelProvider for LeasedModel {
    fn id(&self) -> ProviderId {
        self.model.id()
    }

    async fn list_models(&self) -> Vec<ModelDescriptor> {
        self.model.list_models().await
    }

    async fn stream(
        &self,
        request: AuthorizedModelRequest,
        cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<ModelEvent>, ErrorEnvelope> {
        let deadline = Duration::from_millis(request.request().limits.deadline_ms);
        let reservation = self.leases.reserve(&self.lease_id, deadline).await?;
        let mut upstream = self.model.stream(request, cancel).await?;
        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            let _held = reservation;
            while let Some(event) = upstream.recv().await {
                if sender.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(receiver)
    }
}
