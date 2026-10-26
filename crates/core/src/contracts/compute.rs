//! Rented compute, paid by the minute (ADR 0072).
//!
//! A lease is a model endpoint on someone else's GPU: a serverless endpoint
//! first, a rented machine later. These types are what core knows about one;
//! no provider's own shapes appear here. Money is whole micro-dollars so a cap
//! is compared exactly, never as a float.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{LeaseId, ProviderConfig, ProviderId, SCHEMA_VERSION, Timestamp};

/// What to rent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputeSpec {
    /// Provider GPU kinds, in order of preference.
    pub gpu_types: Vec<String>,
    /// The serving image, such as RunPod's `worker-vllm`, by pinned tag.
    pub image: String,
    /// The model the image serves.
    pub model_id: String,
    /// Provider region, when the person chose one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub region: Option<String>,
    /// The highest rate the person accepted, in micro-dollars per hour.
    pub max_rate_micro_usd_per_hour: u64,
    /// After this many seconds with no request, the provider itself scales
    /// the lease to zero — the stop that holds when Tramontana is gone.
    pub idle_limit_secs: u32,
}

/// A lease the provider accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputeLease {
    /// The provider's identifier for it.
    pub lease_id: LeaseId,
    /// Who is renting it out.
    pub provider_id: ProviderId,
    /// What it costs while it runs, in micro-dollars per hour.
    pub rate_micro_usd_per_hour: u64,
}

/// Where a lease is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "camelCase")]
#[non_exhaustive]
pub enum LeaseStatus {
    /// Accepted, not yet serving.
    Starting,
    /// Serving. The endpoint is an ordinary OpenAI-compatible one.
    #[serde(rename_all = "camelCase")]
    Ready {
        /// How to reach it, credential by reference only.
        endpoint: ProviderConfig,
        /// Workers the provider keeps running with no requests. Anything
        /// above zero bills around the clock and is warned about.
        always_on_workers: u32,
    },
    /// Being terminated.
    Stopping,
    /// Terminated; nothing is billing.
    Stopped,
    /// The provider gave up on it.
    #[serde(rename_all = "camelCase")]
    Failed {
        /// Secret-free description.
        reason: String,
    },
}

/// What a lease has cost, as the provider reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaseCost {
    /// Billed so far, in micro-dollars.
    pub spent_micro_usd: u64,
    /// Current rate, in micro-dollars per hour.
    pub rate_micro_usd_per_hour: u64,
}

/// Why a lease was terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LeaseStopReason {
    /// No request for the idle limit.
    Idle,
    /// The spending cap was reached.
    Cap,
    /// The person stopped it.
    Person,
    /// It failed, or its cost could not be read.
    Failure,
    /// The provider or the machine stopped it without Tramontana.
    Watchdog,
    /// Tramontana closed and stopped what it had running.
    AppClosed,
}

/// A durable lease event. Every one carries the rate, so the record alone
/// says what a lease cost per hour while it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ComputeLeaseEvent {
    /// The provider accepted a lease.
    #[serde(rename_all = "camelCase")]
    ComputeLeaseStarted {
        /// Contract schema version.
        schema_version: u32,
        /// The lease.
        lease_id: LeaseId,
        /// Who rents it out.
        provider_id: ProviderId,
        /// The model it serves.
        model_id: String,
        /// Micro-dollars per hour.
        rate_micro_usd_per_hour: u64,
        /// When.
        occurred_at: Timestamp,
    },
    /// The lease is serving.
    #[serde(rename_all = "camelCase")]
    ComputeLeaseReady {
        /// Contract schema version.
        schema_version: u32,
        /// The lease.
        lease_id: LeaseId,
        /// Micro-dollars per hour.
        rate_micro_usd_per_hour: u64,
        /// Workers kept running with no requests; above zero is warned about.
        always_on_workers: u32,
        /// When.
        occurred_at: Timestamp,
    },
    /// The lease was terminated.
    #[serde(rename_all = "camelCase")]
    ComputeLeaseStopped {
        /// Contract schema version.
        schema_version: u32,
        /// The lease.
        lease_id: LeaseId,
        /// Why.
        reason: LeaseStopReason,
        /// Micro-dollars per hour.
        rate_micro_usd_per_hour: u64,
        /// What it had cost, as far as Tramontana knew.
        spent_micro_usd: u64,
        /// When.
        occurred_at: Timestamp,
    },
}

impl ComputeLeaseEvent {
    /// A started event.
    #[must_use]
    pub fn started(lease: &ComputeLease, model_id: &str, occurred_at: Timestamp) -> Self {
        Self::ComputeLeaseStarted {
            schema_version: SCHEMA_VERSION,
            lease_id: lease.lease_id.clone(),
            provider_id: lease.provider_id.clone(),
            model_id: model_id.to_owned(),
            rate_micro_usd_per_hour: lease.rate_micro_usd_per_hour,
            occurred_at,
        }
    }

    /// The lease this event is about.
    #[must_use]
    pub fn lease_id(&self) -> &LeaseId {
        match self {
            Self::ComputeLeaseStarted { lease_id, .. }
            | Self::ComputeLeaseReady { lease_id, .. }
            | Self::ComputeLeaseStopped { lease_id, .. } => lease_id,
        }
    }
}

/// What `millis` of running at `rate_micro_usd_per_hour` costs, rounded up so
/// a cap is never undercounted.
#[must_use]
pub fn cost_of_millis(rate_micro_usd_per_hour: u64, millis: u64) -> u64 {
    let cost = u128::from(rate_micro_usd_per_hour) * u128::from(millis);
    u64::try_from(cost.div_ceil(3_600_000)).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn cost_rounds_up_and_saturates() {
        // $3.60 an hour is a micro-dollar a millisecond.
        assert_eq!(cost_of_millis(3_600_000, 1), 1);
        assert_eq!(cost_of_millis(1, 1), 1);
        assert_eq!(cost_of_millis(0, 1_000), 0);
        assert_eq!(cost_of_millis(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn events_are_tagged_camel_case() {
        let event = ComputeLeaseEvent::ComputeLeaseStopped {
            schema_version: SCHEMA_VERSION,
            lease_id: LeaseId::new("l1"),
            reason: LeaseStopReason::Cap,
            rate_micro_usd_per_hour: 2_000_000,
            spent_micro_usd: 10,
            occurred_at: Timestamp::new("2026-01-01T00:00:00Z"),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "computeLeaseStopped");
        assert_eq!(json["reason"], "cap");
        assert_eq!(json["rateMicroUsdPerHour"], 2_000_000);
    }
}
