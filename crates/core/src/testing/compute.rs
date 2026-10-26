use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use crate::contracts::{
    ComputeLease, ComputeLeaseEvent, ComputeSpec, CorrelationId, ErrorCode, ErrorEnvelope,
    LeaseCost, LeaseId, LeaseStatus, ProviderConfig, ProviderId,
};
use crate::ports::{ComputeLeaseJournal, ComputeProvisioner};

/// A provider that rents out pretend GPUs, for free.
///
/// It behaves like a serverless provider: it bills while a request arrived
/// within the spec's idle limit, and scales to zero on its own after that —
/// unless a test gives it always-on workers.
#[derive(Debug)]
pub struct FakeComputeProvisioner {
    state: Mutex<FakeCompute>,
}

#[derive(Debug)]
struct FakeCompute {
    rate: u64,
    cost_readable: bool,
    always_on_workers: u32,
    starting_polls: u32,
    created: u32,
    stops: u32,
    leases: HashMap<LeaseId, FakeLease>,
}

#[derive(Debug)]
struct FakeLease {
    spec: ComputeSpec,
    polls: u32,
    stopped: bool,
    spent: u64,
    last_request: Instant,
}

fn unavailable(message: &str) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::ProviderUnavailable,
        message,
        CorrelationId::new("fake-compute"),
    )
}

impl FakeComputeProvisioner {
    /// A provider charging `rate` micro-dollars an hour, ready on the second
    /// status read.
    #[must_use]
    pub fn new(rate_micro_usd_per_hour: u64) -> Self {
        Self {
            state: Mutex::new(FakeCompute {
                rate: rate_micro_usd_per_hour,
                cost_readable: true,
                always_on_workers: 0,
                starting_polls: 1,
                created: 0,
                stops: 0,
                leases: HashMap::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, FakeCompute> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether `cost` answers.
    pub fn set_cost_readable(&self, readable: bool) {
        self.state().cost_readable = readable;
    }

    /// Keep workers warm, as a person might in the provider's console.
    pub fn set_always_on_workers(&self, workers: u32) {
        self.state().always_on_workers = workers;
    }

    /// What the provider's billing says a lease has cost.
    pub fn set_spent(&self, lease: &LeaseId, spent_micro_usd: u64) {
        if let Some(lease) = self.state().leases.get_mut(lease) {
            lease.spent = spent_micro_usd;
        }
    }

    /// A request reached the lease's endpoint.
    pub fn request(&self, lease: &LeaseId) {
        if let Some(lease) = self.state().leases.get_mut(lease) {
            lease.last_request = Instant::now();
        }
    }

    /// The provider or the machine stops a lease on its own.
    pub fn stop_on_provider_side(&self, lease: &LeaseId) {
        if let Some(lease) = self.state().leases.get_mut(lease) {
            lease.stopped = true;
        }
    }

    /// Whether the lease is billing right now, with nobody watching it.
    #[must_use]
    pub fn billing(&self, lease: &LeaseId) -> bool {
        let state = self.state();
        state.leases.get(lease).is_some_and(|lease| {
            !lease.stopped
                && (state.always_on_workers > 0
                    || lease.last_request.elapsed()
                        < Duration::from_secs(u64::from(lease.spec.idle_limit_secs)))
        })
    }

    /// Whether the lease was terminated.
    #[must_use]
    pub fn stopped(&self, lease: &LeaseId) -> bool {
        self.state()
            .leases
            .get(lease)
            .is_some_and(|lease| lease.stopped)
    }

    /// How many leases were created.
    #[must_use]
    pub fn created(&self) -> u32 {
        self.state().created
    }

    /// How many stop calls arrived, including repeats.
    #[must_use]
    pub fn stop_calls(&self) -> u32 {
        self.state().stops
    }
}

#[async_trait]
impl ComputeProvisioner for FakeComputeProvisioner {
    fn id(&self) -> ProviderId {
        ProviderId::new("fake-compute")
    }

    async fn provision(&self, spec: &ComputeSpec) -> Result<ComputeLease, ErrorEnvelope> {
        let mut state = self.state();
        state.created += 1;
        let lease_id = LeaseId::new(format!("lease-{}", state.created));
        state.leases.insert(
            lease_id.clone(),
            FakeLease {
                spec: spec.clone(),
                polls: 0,
                stopped: false,
                spent: 0,
                last_request: Instant::now(),
            },
        );
        Ok(ComputeLease {
            lease_id,
            provider_id: ProviderId::new("fake-compute"),
            rate_micro_usd_per_hour: state.rate,
        })
    }

    async fn status(&self, lease_id: &LeaseId) -> Result<LeaseStatus, ErrorEnvelope> {
        let mut state = self.state();
        let starting_polls = state.starting_polls;
        let always_on_workers = state.always_on_workers;
        let lease = state
            .leases
            .get_mut(lease_id)
            .ok_or_else(|| unavailable("no such lease"))?;
        if lease.stopped {
            return Ok(LeaseStatus::Stopped);
        }
        lease.polls += 1;
        if lease.polls <= starting_polls {
            return Ok(LeaseStatus::Starting);
        }
        Ok(LeaseStatus::Ready {
            endpoint: ProviderConfig::OpenAiCompatible {
                base_url: format!("https://compute.invalid/{lease_id}/openai"),
                model_id: lease.spec.model_id.clone(),
                credential: None,
            },
            always_on_workers,
        })
    }

    async fn stop(&self, lease_id: &LeaseId) -> Result<(), ErrorEnvelope> {
        let mut state = self.state();
        state.stops += 1;
        if let Some(lease) = state.leases.get_mut(lease_id) {
            lease.stopped = true;
        }
        Ok(())
    }

    async fn cost(&self, lease_id: &LeaseId) -> Result<LeaseCost, ErrorEnvelope> {
        let state = self.state();
        if !state.cost_readable {
            return Err(unavailable("billing is not answering"));
        }
        let lease = state
            .leases
            .get(lease_id)
            .ok_or_else(|| unavailable("no such lease"))?;
        Ok(LeaseCost {
            spent_micro_usd: lease.spent,
            rate_micro_usd_per_hour: state.rate,
        })
    }
}

/// Lease events, in memory.
#[derive(Debug, Default)]
pub struct InMemoryComputeLeaseJournal {
    events: Mutex<Vec<ComputeLeaseEvent>>,
}

#[async_trait]
impl ComputeLeaseJournal for InMemoryComputeLeaseJournal {
    async fn append(&self, event: &ComputeLeaseEvent) -> Result<(), ErrorEnvelope> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ComputeLeaseEvent>, ErrorEnvelope> {
        Ok(self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }
}
