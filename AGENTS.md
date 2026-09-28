# Repository instructions for coding agents

These instructions apply to every file in this repository unless a more specific `AGENTS.md` exists deeper in the tree.

## Mission and current scope

Build Tramontana Phase 1 as the portable, trustworthy control plane for personal AI work on macOS, Windows, and Linux. The product shell and durable project/task model must remain independent of model location and specialist worker. Phase 1 is a personal desktop application; enterprise control planes, centralized inference, workstation pools, and distributed compute are later work.

Read these before implementing architecture or security-sensitive work:

1. `docs/BUILD_ORDER.md` — current milestone, dependencies, and exit gate
2. `docs/BETA_1_SCOPE.md` — first beta boundary; it does not replace Phase 1 acceptance
3. `docs/PHASE_1_SPEC.md`
4. `docs/STRATEGIC_POSITIONING.md`
5. `docs/ARCHITECTURE.md`
6. `docs/CORE_CONTRACTS.md`
7. `docs/PROJECT_FORMAT.md`
8. `docs/SECURITY_AND_PERMISSIONS.md`
9. `docs/CONFIGURATION.md`
10. `docs/DEVELOPMENT.md`
11. `docs/TESTING_STRATEGY.md`
12. `docs/DEPENDENCY_ADOPTION.md`
13. `docs/PHASE_1_ACCEPTANCE_CRITERIA.md`

For a narrow change, read the current milestone plus all documents governing the affected boundary; do not treat this list as permission to skip relevant contracts.

Before implementing or materially changing the desktop UI, read `docs/UI_DESIGN_REFERENCE.md` and inspect `design/UI/Tramontana Design System v2/` (the earlier `design/UI/Tramontana UI Design Specification/` is superseded). The supplied design and frontend template are the preferred visual and interaction direction, but they are a strong suggestion rather than a normative product or architecture contract.

These documents were written before the code existed and some of them are wrong. They are strong defaults, not law. When one contradicts reality, fix it in the same change and note it in the milestone evidence — see `docs/BUILD_ORDER.md` §3. The exceptions are the boundaries in `docs/SECURITY_AND_PERMISSIONS.md` and the rule that shipped capability is described honestly.

## Architectural boundaries

- `apps/desktop` owns presentation, user interaction, and Tauri command adapters. It must not contain model-routing policy, permission policy, secret storage, or provider business logic.
- `crates/core` owns provider-neutral domain types and application services: runtime orchestration, routing, permissions, context construction, task state, tool registry, and persistence ports.
- Concrete local, cloud, tool, and persistence integrations implement core ports. Provider SDK types must not leak into core domain interfaces or persisted conversation state.
- `packages/shared` contains versioned schemas/types generated or mirrored from the core boundary. Do not create a second source of truth for domain rules.
- All side effects cross explicit interfaces. UI components never spawn unrestricted commands, read arbitrary files, or call model APIs directly.
- Keep model selection separate from compute placement. `ModelRouter` chooses model/provider capabilities; `ComputeRouter` chooses an execution target. Phase 1 may map both to the same local process, but their contracts stay distinct.
- Treat `AgentProvider` as the stable worker boundary for internal and external specialist agents. Tramontana owns task identity, lifecycle, approvals, audit, context-release policy, and normalized events; workers own their specialist execution strategy.
- Do not duplicate mature coding-agent runtimes without an explicit reason. Prefer adapters for Codex, Goose, OpenCode, OpenHands, Claude Code, or ACP-compatible agents where their process/tool behavior can be governed through Tramontana's policy boundary.
- Tramontana owns the versioned project/work format: source references, instructions, skills, policy references, memory/artifact references, task records, and handoff checkpoints. No worker-specific session format is the canonical record.
- Tools, skills, workflows, and agent definitions are model-provider-neutral.
- MCP is the primary third-party tool/App integration boundary. Avoid one-off integration APIs in the runtime when an MCP adapter is appropriate.
- `apps/cli` and `apps/desktop` are peer adapters over the same Rust application services. Never duplicate policy or task behavior in the CLI to make a milestone demonstration pass.

## Build versus integrate

- **Build and own:** portable project/work format, desktop work surface, task/event state, handoff/checkpoints, routing policy, permissions, approvals, secrets references, audit/provenance, and stable model/worker/execution interfaces.
- **Integrate behind adapters:** local inference runtimes, cloud APIs/gateways, coding-agent workers, browser/computer drivers, and enterprise retrieval/connectors.
- **Monitor or prototype later:** workstation pools, split-model inference, and distributed runtimes. They must not shape Phase 1 implementation details beyond the stable compute interface.

If an implementation needs to violate one of these boundaries, stop and propose an architecture change before coding it.

## Implementation approach

- Make small, reviewable vertical slices that leave the repository buildable.
- Follow `docs/BUILD_ORDER.md`. Work on a later milestone only when its prerequisites pass or an architecture decision records the exception.
- Do not cross a validation gate (`V1`–`V6`) without its evidence note. A gate exists to answer one question before more work is stacked on the answer; hitting a gate's re-plan trigger is a result, not a failure, and it is reported rather than worked around.
- Prefer explicit types, deterministic state transitions, structured events, and dependency injection over global state or hidden side effects.
- Keep Tauri commands thin and version their request/response payloads.
- Design long-running work as cancellable tasks with progress events and durable state.
- Design worker sessions as supervised, replaceable executions. Normalize their events and checkpoint enough state to stop, inspect, and resume or hand off without making a worker's private transcript canonical.
- Use deterministic filesystem/search/edit/Git/test tools before visual computer control.
- Feature-detect model and tool capabilities; do not infer them from provider names.
- Preserve unknown fields where practical when loading forward-compatible configuration.
- Use migrations for persisted data. Never silently discard or rewrite user conversations/configuration.

## UI design reference

- Use `design/UI/Tramontana Design System v2/` as the starting point for layout, visual hierarchy, branding, component language, and interaction intent.
- Do not treat generated HTML, inline styles, demo data, or prototype behavior as production architecture. Reimplement the design in the repository's React/Tauri structure and shared component system.
- Product behavior, security boundaries, accessibility, honest capability disclosure, responsive behavior, and native platform constraints take precedence over the mockup.
- Preserve the design direction where practical. If implementation evidence supports a meaningful deviation, document the reason in the change; update the design reference only when the user intentionally revises it.

## Code quality and tests

- Format, lint, type-check, and test changed code before handing off.
- Rust: `cargo fmt`, `cargo clippy` with warnings denied for first-party code, and `cargo test`.
- TypeScript/React: formatter, ESLint, strict TypeScript checking, and unit/component tests.
- Add contract tests for every provider adapter and permission boundary.
- Add integration tests for routing decisions, cancellation, tool approval, context minimization, and failure recovery.
- Tests must not require paid APIs by default. Cloud adapter tests use fakes/recorded fixtures unless explicitly enabled by a developer.
- Avoid snapshot tests for security policy and business rules when direct assertions are clearer.
- Any bug fix includes a regression test when practical.
- Default automated tests are offline, secret-free, and free of paid API calls. Follow the contract, security, concurrency, and platform matrices in `docs/TESTING_STRATEGY.md`.

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

When a change affects component ownership, the project/work format, stable interfaces, event schemas, worker lifecycle, handoff, routing, permissions, persistence, configuration precedence, or Phase 1 scope, update the relevant files under `docs/` in the same commit.

For a significant architecture, dependency, security, compatibility, or acceptance exception, add a record under `docs/decisions/` using its template. Keep the short architectural summary in `docs/ARCHITECTURE.md` when it helps readers. Do not let code silently become the architecture.

When adopting or studying an upstream repository, follow `docs/DEPENDENCY_ADOPTION.md`. Do not clone, vendor, add a submodule, copy source, download an executable, or make a package a production dependency merely because it appears in `OPEN_SOURCE_FOUNDATIONS.md`.

Update `docs/PHASE_1_ACCEPTANCE_CRITERIA.md` only when the product contract intentionally changes, not to make incomplete work appear complete.

## Git and change safety

- Inspect the worktree before editing and preserve unrelated user changes.
- Do not rewrite history, force-push, delete branches, or discard uncommitted work unless the user explicitly asks.
- Keep generated files and dependency lockfiles intentional. Commit lockfiles for reproducible application builds.
- Use clear commit messages that describe the delivered slice.

## Definition of handoff

Report what changed, which checks ran, any checks not run, remaining risks, and any architecture/documentation decisions. A task is not complete merely because code was written; it must satisfy the relevant acceptance criteria and preserve the security boundaries above.

For milestone work, report the milestone exit criteria individually. Do not claim that Beta 1 or Phase 1 is complete when only the current slice passes.

For a validation gate, answer the gate's question in plain language and say what the evidence actually showed — a list of passing checks is not an answer.
