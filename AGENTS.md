# Repository instructions for coding agents

These instructions apply to every file in this repository unless a more specific `AGENTS.md` exists deeper in the tree.

## Mission

Tramontana lets people in industries where privacy is critical use AI models and agents on confidential documents and systems, with the model running on their own computer or their company's own server. Every action is approved, every step is recorded, and models and agents stay replaceable. It is a desktop application for macOS, Windows, and Linux, and a CLI.

## Architectural boundaries

- `apps/desktop` owns presentation, user interaction, and Tauri command adapters. It must not contain model-routing policy, permission policy, secret storage, or provider business logic.
- `crates/core` owns provider-neutral domain types and application services: runtime orchestration, routing, permissions, context construction, task state, tool registry, and persistence ports.
- Concrete local, cloud, tool, and persistence integrations implement core ports. Provider SDK types must not leak into core domain interfaces or persisted conversation state.
- `packages/shared` contains versioned schemas/types generated from the core boundary. Do not create a second source of truth for domain rules.
- All side effects cross explicit interfaces. UI components never spawn unrestricted commands, read arbitrary files, or call model APIs directly.
- Keep model selection separate from compute placement. `ModelRouter` chooses model/provider capabilities; `ComputeRouter` chooses an execution target. They may map to the same local process, but their contracts stay distinct.
- Treat `AgentProvider` as the stable worker boundary for internal and external specialist agents. Tramontana owns task identity, lifecycle, approvals, audit, context-release policy, and normalized events; workers own their specialist execution strategy.
- Tramontana owns the versioned project/work format: source references, instructions, skills, policy references, memory/artifact references, task records, and handoff checkpoints. No worker-specific session format is the canonical record.
- Tools, skills, workflows, and agent definitions are model-provider-neutral.
- MCP is the primary third-party tool/App integration boundary. Avoid one-off integration APIs in the runtime when an MCP adapter is appropriate.
- `apps/cli` and `apps/desktop` are peer adapters over the same Rust application services. Never duplicate policy or task behavior in one of them.

If an implementation needs to violate one of these boundaries, stop and propose an architecture change before coding it.

## Build versus integrate

- **Build and own:** project/work format, desktop work surface, task/event state, handoff/checkpoints, routing policy, permissions, approvals, secrets references, audit/provenance, and stable model/worker/execution interfaces.
- **Integrate behind adapters:** local inference runtimes, cloud APIs/gateways, coding-agent workers, browser/computer drivers, and enterprise retrieval/connectors.

## Implementation approach

- Make small, reviewable vertical slices that leave the repository buildable.
- Prefer explicit types, deterministic state transitions, structured events, and dependency injection over global state or hidden side effects.
- Keep Tauri commands thin and version their request/response payloads.
- Design long-running work as cancellable tasks with progress events and durable state.
- Design worker sessions as supervised, replaceable executions. Normalize their events and checkpoint enough state to stop, inspect, and resume or hand off without making a worker's private transcript canonical.
- Use deterministic filesystem/search/edit/Git/test tools before visual computer control.
- Feature-detect model and tool capabilities; do not infer them from provider names.
- Preserve unknown fields where practical when loading forward-compatible configuration.
- Use migrations for persisted data. Never silently discard or rewrite user conversations/configuration.
- Describe shipped capability honestly, in the README and in the interface.

## Code quality and tests

- Format, lint, type-check, and test changed code before handing off.
- Rust: `cargo fmt`, `cargo clippy` with warnings denied for first-party code, and `cargo test`.
- TypeScript/React: formatter, ESLint, strict TypeScript checking, and unit/component tests.
- Add contract tests for every provider adapter and permission boundary.
- Add integration tests for routing decisions, cancellation, tool approval, context minimization, and failure recovery.
- Default automated tests are offline, secret-free, and free of paid API calls. Cloud adapter tests use fakes/recorded fixtures unless explicitly enabled by a developer.
- Avoid snapshot tests for security policy and business rules when direct assertions are clearer.
- Any bug fix includes a regression test when practical.

## Security and privacy

- Never commit secrets, credentials, tokens, private keys, user data, model binaries, or real conversation transcripts.
- `.env` is local-only. `.env.example` contains placeholders and safe comments only.
- Use the operating system credential store for persisted secrets. Do not put secrets in Markdown configuration, app logs, telemetry, crash reports, URLs, or persisted task events.
- Filesystem access is project-scoped and canonicalized. Defend against symlink/path traversal and deny access outside granted roots.
- No unrestricted host shell by default. Commands require policy evaluation and dangerous commands require explicit approval.
- Every tool invocation carries origin, requested capability, scope, risk class, routing destination, and approval result in an auditable event.
- Never send local files or derived context to a cloud provider without a visible, permission-aware routing decision. Minimize payloads and disclose the destination before transmission.
- Treat MCP servers and their returned content as untrusted. Validate schemas, constrain transport/process launch, and apply the same permission system as built-in tools.
- Redact secrets in logs and error messages. Fail closed when permission state is missing or ambiguous.

## Git and change safety

- Inspect the worktree before editing and preserve unrelated user changes.
- Do not rewrite history, force-push, delete branches, or discard uncommitted work unless the user explicitly asks.
- Keep generated files and dependency lockfiles intentional. Commit lockfiles for reproducible application builds.
- Use clear commit messages that describe the delivered slice.

## Definition of handoff

Report what changed, which checks ran, any checks not run, and remaining risks. A task is not complete merely because code was written; it must preserve the security boundaries above.
