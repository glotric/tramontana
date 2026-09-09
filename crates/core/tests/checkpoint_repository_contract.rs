//! The in-memory `CheckpointRepository` runs the shared contract suite.
//!
//! The suite lives in `tramontana_core::testing::contract_suites` so that
//! `tramontana-persistence` runs the identical cases against SQLite.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::sync::Arc;

use tramontana_core::ports::CheckpointRepository;
use tramontana_core::testing::InMemoryCheckpointRepository;
use tramontana_core::testing::contract_suites::checkpoint_repository_contract;

#[tokio::test]
async fn the_in_memory_repository_obeys_the_checkpoint_contract() {
    checkpoint_repository_contract(|| async {
        Arc::new(InMemoryCheckpointRepository::new()) as Arc<dyn CheckpointRepository>
    })
    .await;
}
