//! Deterministic in-memory implementations of every port.
//!
//! `TESTING_STRATEGY.md` §3 requires reusable fakes that implement the same
//! public ports as real adapters, with no test-only shortcuts in the runtime.
//! These are that set.
//!
//! They are compiled into the library rather than hidden behind `cfg(test)`
//! because at M1 they are the *only* implementations: the CLI and the desktop
//! shell both drive a real task through fakes, which is what makes the walking
//! skeleton walk. When real adapters arrive they should move behind a Cargo
//! feature or into a separate crate so they cannot ship in a release binary.

pub mod contract_suites;
pub mod release;

mod clock;
mod event_store;
mod ids;
mod model;
mod permission;
mod receiving_worker;
mod repositories;
mod secret;
mod tool;
mod tool_plane;
mod worker;

pub use clock::{FakeClock, FakeLocalClock};
pub use event_store::InMemoryEventStore;
pub use ids::SequentialIdSource;
pub use model::{FakeModelProvider, FakeModelScript};
pub use permission::{InMemoryApprovalStore, ScriptedApprover};
pub use receiving_worker::{HANDOFF_RECEIVED, RECEIVING_AGENT_ID, ReceivingWorker};
pub use repositories::{
    FakeTranscriber, InMemoryCheckpointRepository, InMemoryConfigurationRepository,
    InMemoryConversationRepository, InMemoryDictationRepository, InMemoryPersonalRepository,
    InMemoryProjectRepository, InMemoryScheduleRepository, InMemoryTaskRepository,
};
pub use secret::InMemorySecretVault;
pub use tool::{FAKE_TOOL_ID, FAKE_WRITE_TOOL_ID, FakeToolProvider, FakeWriteTool};
pub use tool_plane::{AssembledPlane, assemble_tool_plane};
pub use worker::{FAKE_AGENT_ID, FakeWorker};
