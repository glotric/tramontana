# Desktop application

Intended home of the Tauri desktop shell and React + TypeScript UI.

When implementation starts, keep this package focused on presentation, accessibility, interaction state, and thin versioned Tauri commands/events. It should make projects, workers, model/compute destinations, context release, approvals, checkpoints, handoffs, and execution provenance visible. Routing, permissions, context, secrets, worker/provider logic, persistence policy, filesystem access, and process execution belong behind Rust core interfaces.

The initial scaffold should be created as the first vertical slice described in `docs/PHASE_1_SPEC.md`; pin toolchain/dependency versions and commit lockfiles.
