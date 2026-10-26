//! Rented compute: every lease stops, and the cap holds (ADR 0072).
//!
//! Against the fake provider, offline and free. Time is tokio's paused clock,
//! so an hour of idling costs nothing to wait out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    ComputeLeaseEvent, ComputeSpec, ErrorCode, LeaseId, LeaseStopReason, ModelEvent,
};
use tramontana_core::ports::{ComputeLeaseJournal, ModelProvider};
use tramontana_core::services::{ComputeLeases, LeasePolicy, LeasedModel};
use tramontana_core::testing::contract_suites::model_request;
use tramontana_core::testing::release::locally_authorized;
use tramontana_core::testing::{
    FakeClock, FakeComputeProvisioner, FakeModelProvider, InMemoryComputeLeaseJournal,
};

/// $3.60 an hour: one micro-dollar a millisecond, so sums are readable.
const RATE: u64 = 3_600_000;
/// A one-dollar cap.
const CAP: u64 = 1_000_000;

fn spec() -> ComputeSpec {
    ComputeSpec {
        gpu_types: vec!["NVIDIA H100 80GB HBM3".to_owned()],
        image: "runpod/worker-v1-vllm:v2.0.0".to_owned(),
        model_id: "Qwen/Qwen3-32B".to_owned(),
        region: None,
        max_rate_micro_usd_per_hour: RATE,
        idle_limit_secs: 30,
    }
}

struct Rig {
    provider: Arc<FakeComputeProvisioner>,
    journal: Arc<InMemoryComputeLeaseJournal>,
    leases: ComputeLeases,
}

fn rig(rate: u64) -> Rig {
    let provider = Arc::new(FakeComputeProvisioner::new(rate));
    let journal = Arc::new(InMemoryComputeLeaseJournal::default());
    let leases = ComputeLeases::new(
        provider.clone(),
        journal.clone(),
        Arc::new(FakeClock::new()),
        LeasePolicy {
            cap_micro_usd: CAP,
            idle_limit: Duration::from_secs(60),
            check_every: Duration::from_secs(5),
            ready_timeout: Duration::from_secs(300),
        },
    );
    Rig {
        provider,
        journal,
        leases,
    }
}

async fn wait(seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}

/// The reasons the journal recorded for stopping `lease`.
async fn stops(journal: &InMemoryComputeLeaseJournal, lease: &LeaseId) -> Vec<LeaseStopReason> {
    journal
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            ComputeLeaseEvent::ComputeLeaseStopped {
                lease_id, reason, ..
            } if &lease_id == lease => Some(reason),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_unused_lease_stops_on_idle_and_every_event_carries_the_rate() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();

    wait(59).await;
    assert_eq!(rig.leases.running(), vec![ready.lease_id.clone()]);
    wait(10).await;

    assert!(rig.leases.running().is_empty());
    assert!(rig.provider.stopped(&ready.lease_id));
    let events = rig.journal.list().await.unwrap();
    assert_eq!(events.len(), 3, "{events:?}");
    for event in &events {
        let rate = match event {
            ComputeLeaseEvent::ComputeLeaseStarted {
                rate_micro_usd_per_hour,
                ..
            }
            | ComputeLeaseEvent::ComputeLeaseReady {
                rate_micro_usd_per_hour,
                ..
            }
            | ComputeLeaseEvent::ComputeLeaseStopped {
                rate_micro_usd_per_hour,
                ..
            } => *rate_micro_usd_per_hour,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(rate, RATE);
    }
    assert_eq!(
        stops(&rig.journal, &ready.lease_id).await,
        vec![LeaseStopReason::Idle]
    );
}

#[tokio::test(start_paused = true)]
async fn a_call_in_flight_is_not_idle() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();

    let call = rig
        .leases
        .reserve(&ready.lease_id, Duration::from_secs(600))
        .await
        .unwrap();
    wait(120).await;
    assert_eq!(rig.leases.running().len(), 1);

    drop(call);
    wait(70).await;
    assert!(rig.leases.running().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_lease_stops_when_the_provider_bills_up_to_the_cap() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();

    rig.provider.set_spent(&ready.lease_id, CAP);
    wait(6).await;

    assert!(rig.provider.stopped(&ready.lease_id));
    assert_eq!(
        stops(&rig.journal, &ready.lease_id).await,
        vec![LeaseStopReason::Cap]
    );
    let refused = rig.leases.start(&spec()).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::LimitExceeded);
    assert_eq!(rig.provider.created(), 1);
}

#[tokio::test(start_paused = true)]
async fn concurrent_calls_cannot_overshoot_the_cap() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();
    // Ten minutes at a micro-dollar a millisecond: 600,000 of the 1,000,000.
    let deadline = Duration::from_secs(600);

    let (first, second) = tokio::join!(
        rig.leases.reserve(&ready.lease_id, deadline),
        rig.leases.reserve(&ready.lease_id, deadline),
    );
    let (held, refused) = match (first, second) {
        (Ok(held), Err(refused)) | (Err(refused), Ok(held)) => (held, refused),
        other => panic!("exactly one call fits under the cap: {other:?}"),
    };
    assert_eq!(refused.code, ErrorCode::LimitExceeded);
    assert_eq!(rig.leases.committed_micro_usd(), 600_000);
    // A call that is refused while another runs does not stop the lease.
    assert_eq!(rig.leases.running().len(), 1);

    // The call ran ten seconds: the rest of its reservation comes back.
    wait(10).await;
    drop(held);
    assert_eq!(rig.leases.committed_micro_usd(), 10_000);
    let again = rig.leases.reserve(&ready.lease_id, deadline).await;
    assert!(again.is_ok());
}

#[tokio::test(start_paused = true)]
async fn an_idle_lease_that_cannot_afford_a_call_stops_on_cap() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();

    let refused = rig
        .leases
        .reserve(&ready.lease_id, Duration::from_secs(2_000))
        .await
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::LimitExceeded);
    assert!(rig.provider.stopped(&ready.lease_id));
    assert_eq!(
        stops(&rig.journal, &ready.lease_id).await,
        vec![LeaseStopReason::Cap]
    );
}

#[tokio::test(start_paused = true)]
async fn the_provider_scales_to_zero_when_tramontana_is_gone() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();
    rig.provider.request(&ready.lease_id);
    assert!(rig.provider.billing(&ready.lease_id));

    // The provider's own idle limit (30 s) is shorter than Tramontana's
    // (60 s), so this is the provider stopping the bill, with no call from
    // Tramontana: what still holds when the laptop sleeps or the app dies.
    wait(31).await;
    assert!(!rig.provider.billing(&ready.lease_id));
    assert!(!rig.provider.stopped(&ready.lease_id));
    assert_eq!(rig.provider.stop_calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn closing_tramontana_terminates_every_lease() {
    let rig = rig(RATE);
    let first = rig.leases.start(&spec()).await.unwrap();
    let second = rig.leases.start(&spec()).await.unwrap();

    rig.leases.close().await;

    for lease in [&first.lease_id, &second.lease_id] {
        assert!(rig.provider.stopped(lease));
        assert_eq!(
            stops(&rig.journal, lease).await,
            vec![LeaseStopReason::AppClosed]
        );
    }
    let refused = rig.leases.start(&spec()).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn always_on_workers_are_reported_because_they_bill_while_idle() {
    let rig = rig(RATE);
    rig.provider.set_always_on_workers(1);
    let ready = rig.leases.start(&spec()).await.unwrap();

    assert_eq!(ready.always_on_workers, 1);
    let events = rig.journal.list().await.unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        ComputeLeaseEvent::ComputeLeaseReady {
            always_on_workers: 1,
            ..
        }
    )));
    wait(31).await;
    assert!(rig.provider.billing(&ready.lease_id));
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_cost_means_no_new_lease() {
    let rig = rig(RATE);
    rig.provider.set_cost_readable(false);

    let refused = rig.leases.start(&spec()).await.unwrap_err();

    assert_eq!(refused.code, ErrorCode::ProviderUnavailable);
    // The one lease the provider created was terminated, not left running.
    let lease = LeaseId::new("lease-1");
    assert!(rig.provider.stopped(&lease));
    assert_eq!(
        stops(&rig.journal, &lease).await,
        vec![LeaseStopReason::Failure]
    );
    assert!(rig.leases.running().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_cost_on_a_running_lease_blocks_the_next_one() {
    let rig = rig(RATE);
    rig.leases.start(&spec()).await.unwrap();
    rig.provider.set_cost_readable(false);

    let refused = rig.leases.start(&spec()).await.unwrap_err();

    assert_eq!(refused.code, ErrorCode::ProviderUnavailable);
    assert_eq!(rig.provider.created(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_rate_above_what_was_accepted_is_terminated() {
    let rig = rig(RATE * 2);

    let refused = rig.leases.start(&spec()).await.unwrap_err();

    assert_eq!(refused.code, ErrorCode::PolicyBlocked);
    assert!(rig.provider.stopped(&LeaseId::new("lease-1")));
}

#[tokio::test(start_paused = true)]
async fn stopping_twice_is_one_stop() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();

    rig.leases.stop(&ready.lease_id).await.unwrap();
    rig.leases.stop(&ready.lease_id).await.unwrap();

    assert_eq!(rig.provider.stop_calls(), 1);
    assert_eq!(
        stops(&rig.journal, &ready.lease_id).await,
        vec![LeaseStopReason::Person]
    );
}

#[tokio::test(start_paused = true)]
async fn a_lease_stopped_by_the_provider_is_recorded_as_the_watchdog() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();

    rig.provider.stop_on_provider_side(&ready.lease_id);
    wait(6).await;

    assert!(rig.leases.running().is_empty());
    assert_eq!(
        stops(&rig.journal, &ready.lease_id).await,
        vec![LeaseStopReason::Watchdog]
    );
}

#[tokio::test(start_paused = true)]
async fn a_leased_model_holds_its_reservation_while_it_streams() {
    let rig = rig(RATE);
    let ready = rig.leases.start(&spec()).await.unwrap();
    let model = LeasedModel::new(
        rig.leases.clone(),
        ready.lease_id.clone(),
        Arc::new(FakeModelProvider::streaming(["hello"])),
    );

    let mut events = model
        .stream(
            locally_authorized(model_request("r1")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(rig.leases.committed_micro_usd() > 0);
    let mut finished = false;
    while let Some(event) = events.recv().await {
        finished |= matches!(event, ModelEvent::Completed { .. });
    }
    assert!(finished);
    // The stream closed in no time on the paused clock: nothing stays held.
    tokio::task::yield_now().await;
    assert_eq!(rig.leases.committed_micro_usd(), 0);
}
