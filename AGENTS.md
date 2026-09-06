# Repository instructions for coding agents

These instructions apply to every file in this repository unless a more specific `AGENTS.md` exists deeper in the tree.

## Mission and current scope

Build Tramontana Phase 1 as a local-first desktop AI and agent environment for macOS, Windows, and Linux. The product shell must remain independent of model location and provider. Phase 1 is a personal desktop application; enterprise control planes, centralized inference, workstation pools, and distributed compute are later work.

Read these before implementing architecture or security-sensitive work:

1. `docs/PHASE_1_SPEC.md`
2. `docs/ARCHITECTURE.md`
3. `docs/SECURITY_AND_PERMISSIONS.md`
4. `docs/CONFIGURATION.md`
5. `docs/PHASE_1_ACCEPTANCE_CRITERIA.md`

## Architectural boundaries

- `apps/desktop` owns presentation, user interaction, and Tauri command adapters. It must not contain model-routing policy, permission policy, secret storage, or provider business logic.
- `crates/core` owns provider-neutral domain types and application services: runtime orchestration, routing, permissions, context construction, task state, tool registry, and persistence ports.
- Concrete local, cloud, tool, and persistence integrations implement core ports. Provider SDK types must not leak into core domain interfaces or persisted conversation state.
- `packages/shared` contains versioned schemas/types generated or mirrored from the core boundary. Do not create a second source of truth for domain rules.
- All side effects cross explicit interfaces. UI components never spawn unrestricted commands, read arbitrary files, or call model APIs directly.
- Keep model selection separate from compute placement. `ModelRouter` chooses model/provider capabilities; `ComputeRouter` chooses an execution target. Phase 1 may map both to the same local process, but their contracts stay distinct.
- Tools, skills, workflows, and agent definitions are model-provider-neutral.
- MCP is the primary third-party tool/App integration boundary. Avoid one-off integration APIs in the runtime when an MCP adapter is appropriate.

If an implementation needs to violate one of these boundaries, stop and propose an architecture change before coding it.

## Implementation approach

- Make small, reviewable vertical slices that leave the repository buildable.
- Prefer explicit types, deterministic state transitions, structured events, and dependency injection over global state or hidden side effects.
- Keep Tauri commands thin and version their request/response payloads.
- Design long-running work as cancellable tasks with progress events and durable state.
- Use deterministic filesystem/search/edit/Git/test tools before visual computer control.
- Feature-detect model and tool capabilities; do not infer them from provider names.
- Preserve unknown fields where practical when loading forward-compatible configuration.
- Use migrations for persisted data. Never silently discard or rewrite user conversations/configuration.

## Code quality and tests

- Format, lint, type-check, and test changed code before handing off.
- Rust: `cargo fmt`, `cargo clippy` with warnings denied for first-party code, and `cargo test`.
- TypeScript/React: formatter, ESLint, strict TypeScript checking, and unit/component tests.
- Add contract tests for every provider adapter and permission boundary.
- Add integration tests for routing decisions, cancellation, tool approval, context minimization, and failure recovery.
- Tests must not require paid APIs by default. Cloud adapter tests use fakes/recorded fixtures unless explicitly enabled by a developer.
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

## Documentation discipline

When a change affects component ownership, stable interfaces, event schemas, routing, permissions, persistence, configuration precedence, or Phase 1 scope, update the relevant files under `docs/` in the same commit.

For a significant architecture decision, add a dated decision section to `docs/ARCHITECTURE.md` (or introduce `docs/decisions/` once decisions become numerous) with context, decision, alternatives, and consequences. Do not let code silently become the architecture.

Update `docs/PHASE_1_ACCEPTANCE_CRITERIA.md` only when the product contract intentionally changes, not to make incomplete work appear complete.

## Git and change safety

- Inspect the worktree before editing and preserve unrelated user changes.
- Do not rewrite history, force-push, delete branches, or discard uncommitted work unless the user explicitly asks.
- Keep generated files and dependency lockfiles intentional. Commit lockfiles for reproducible application builds.
- Use clear commit messages that describe the delivered slice.

## Definition of handoff

Report what changed, which checks ran, any checks not run, remaining risks, and any architecture/documentation decisions. A task is not complete merely because code was written; it must satisfy the relevant acceptance criteria and preserve the security boundaries above.
