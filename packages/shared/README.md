# Shared schemas

Intended home of TypeScript types/validators or generated clients for the versioned Rust-to-UI boundary.

Core schemas—including project/task/event/checkpoint, worker capability, routing, approval, and export records—should have one source of truth. Prefer generation from a reviewed schema/contract over hand-maintaining structurally similar Rust and TypeScript types. Shared code must not duplicate routing, permission, or security policy in the UI layer.
