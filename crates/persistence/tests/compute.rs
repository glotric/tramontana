//! Lease events survive a restart, in order (ADR 0072).

#![allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use tramontana_core::contracts::{
    ComputeLease, ComputeLeaseEvent, LeaseId, LeaseStopReason, ProviderId, SCHEMA_VERSION,
    Timestamp,
};
use tramontana_core::ports::ComputeLeaseJournal;
use tramontana_persistence::{Database, SqliteComputeLeaseJournal};

#[tokio::test]
async fn lease_events_survive_a_restart_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("compute.sqlite");
    let lease = ComputeLease {
        lease_id: LeaseId::new("ep-1"),
        provider_id: ProviderId::new("runpod"),
        rate_micro_usd_per_hour: 2_700_000,
    };
    let events = vec![
        ComputeLeaseEvent::started(
            &lease,
            "Qwen/Qwen3-32B",
            Timestamp::new("2026-10-26T10:00:00Z"),
        ),
        ComputeLeaseEvent::ComputeLeaseStopped {
            schema_version: SCHEMA_VERSION,
            lease_id: lease.lease_id.clone(),
            reason: LeaseStopReason::Idle,
            rate_micro_usd_per_hour: 2_700_000,
            spent_micro_usd: 45_000,
            occurred_at: Timestamp::new("2026-10-26T10:01:00Z"),
        },
    ];

    let journal = SqliteComputeLeaseJournal::new(Database::open(&path).await.unwrap());
    for event in &events {
        journal.append(event).await.unwrap();
    }
    drop(journal);

    let reopened = SqliteComputeLeaseJournal::new(Database::open(&path).await.unwrap());
    assert_eq!(reopened.list().await.unwrap(), events);
}
