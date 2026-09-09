# Tramontana

Tramontana is the portable operating layer for AI work: a local-first, cross-platform desktop environment that keeps projects, instructions, skills, permissions, task history, and execution records understandable and under the user's control while models and specialist agents remain replaceable.

The first product promise is not merely “chat with local models.” It is: **open the same project, use any capable model or agent, and keep the work portable, inspectable, and policy-governed.**

## Phase 1

Phase 1 establishes the personal desktop product for macOS, Windows, and Linux:

- a Tauri desktop shell with a React + TypeScript interface;
- Rust core services for orchestration, permissions, persistence, tools, and model routing;
- local-first inference through `llama.cpp` and MLX on Apple Silicon;
- optional OpenAI, Anthropic, Ollama, and OpenAI-compatible adapters behind stable interfaces;
- a durable task/control plane with visible activity, resumable state, explicit approvals, and execution provenance;
- a worker interface for built-in tools and specialist coding agents, with an ACP-compatible path evaluated and at least one supervised external worker integration;
- MCP-based Apps and Markdown-based skills, agents, rules, and workflows.

Phase 1 has no enterprise server dependency. The default architecture is personal and local, with cloud use optional, visible, policy-controlled, and minimized.

## Status

Milestone M3 is complete. Every tool call now passes a fail-closed permission boundary and lands in the durable record with its origin, capability, scope, and decision — refusals included. Six read-only tools inspect a project's approved roots and nothing else: traversal, absolute paths, symlink escape, and a path repointed mid-read are all refused.

Projects, conversations, tasks, ordered events, and secret-free configuration manifests persist in SQLite and recover on restart. The task runtime still uses deterministic fake providers, which the interface labels explicitly; there is no real model yet, and no modifying tools. Validation gate V1 passed.

```bash
corepack enable pnpm     # Node 24.x; Node 25 removed Corepack
pnpm install
pnpm bootstrap           # validates the toolchain, generates contract artifacts
pnpm check:fast          # format, contracts, lint, types, tests (~5s)
pnpm check               # the above plus the release build
pnpm dev                 # desktop shell
pnpm dev:cli task "Say hello"   # the same task from a terminal
pnpm dev:cli project open /path/to/project
```

Prerequisites, root commands, and known gaps: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

Work is sequenced so the differentiating capability — stop a task, inspect it, resume it safely, hand it to another worker, and export the record — is built and proven in M4 against fakes, before model adapters and workers exist. Validation gates `V1`–`V6` sit between stages; each answers one question with evidence before the next stage begins.

Implementation agents should start in this order:

- [Implementation order and agent execution guide](docs/BUILD_ORDER.md)
- [Beta 1 product contract](docs/BETA_1_SCOPE.md)
- [Development environment and repository contract](docs/DEVELOPMENT.md)
- [Core contracts and event protocol](docs/CORE_CONTRACTS.md)
- [Testing and verification strategy](docs/TESTING_STRATEGY.md)
- [Dependency and repository adoption guide](docs/DEPENDENCY_ADOPTION.md)

Product and architecture references:

- [Phase 1 specification](docs/PHASE_1_SPEC.md)
- [Strategic positioning and build/integrate choices](docs/STRATEGIC_POSITIONING.md)
- [Architecture and stable interfaces](docs/ARCHITECTURE.md)
- [Portable project and work format](docs/PROJECT_FORMAT.md)
- [Security and permissions](docs/SECURITY_AND_PERMISSIONS.md)
- [Configuration conventions](docs/CONFIGURATION.md)
- [Acceptance criteria](docs/PHASE_1_ACCEPTANCE_CRITERIA.md)
- [Roadmap and explicit scope boundaries](docs/ROADMAP.md)
- [Open-source foundations](docs/OPEN_SOURCE_FOUNDATIONS.md)
- [Architecture decision records](docs/decisions/README.md)

## Intended repository layout

```text
apps/desktop/       Tauri application and React user interface
apps/cli/           Headless development and recovery adapter
crates/core/        Provider-neutral Rust domain and application services
packages/shared/    Generated schemas and TypeScript client types
xtask/              Repository build tooling; not shipped
scripts/            Repository checks that are not tests
docs/decisions/     Architecture decision records
.tramontana/        Portable Markdown customization examples
docs/               Product, architecture, security, and delivery contracts
```

Crates for persistence, platform capabilities, and each adapter family are created when their first code lands, not in advance.

Implementation agents must read `AGENTS.md` and the milestone entry in `docs/BUILD_ORDER.md` before making changes. Architecture changes require corresponding documentation and, where specified, a decision record in the same change.

## License

MIT. See `LICENSE`.
