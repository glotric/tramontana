//! Supervised local model runtimes.
//!
//! `ARCHITECTURE.md` §70 names this crate for the `llama.cpp` and MLX process
//! adapters, and `DEPENDENCY_ADOPTION.md` S4 says to adapt `llama.cpp` as a
//! supervised process before considering embedding it: a separate process
//! gives crash isolation, real cancellation, and an upgrade path that does not
//! require rebuilding Tramontana.
//!
//! **Nothing here downloads anything.** `BUILD_ORDER.md` §14 is explicit that
//! automatic binary and model downloads are a separate signed-artifact
//! decision: "Do not silently download executable code or models." The binary
//! and the model are supplied by the user, by path.

pub mod fingerprint;
mod llama;
pub mod runtime_records;

pub use llama::{
    LlamaServerSpec, MINIMUM_BUILD, ProbedVersion, RuntimeKind, StartError, SupervisedLlamaServer,
    mlx_supported, parse_devices, probe_devices, probe_version,
};
