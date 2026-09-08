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

mod clock;
mod event_store;
mod ids;
mod model;
mod repositories;
mod tool;
mod worker;

pub use clock::FakeClock;
pub use event_store::InMemoryEventStore;
pub use ids::SequentialIdSource;
pub use model::{FakeModelProvider, FakeModelScript};
pub use repositories::{
    InMemoryConfigurationRepository, InMemoryConversationRepository, InMemoryProjectRepository,
    InMemoryTaskRepository,
};
pub use tool::{FAKE_TOOL_ID, FakeToolProvider};
pub use worker::{FAKE_AGENT_ID, FakeWorker};
