//! Rented-compute adapters (ADR 0072).
//!
//! `ARCHITECTURE.md` §2: this crate knows one provider's control API and
//! nothing about whether a lease *should* run. Starting, the cap, idle and
//! every stop decision are `tramontana_core::services::ComputeLeases`'.

mod runpod;

pub use runpod::RunPodServerless;
