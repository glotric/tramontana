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

Milestone M6's read-only reference worker is implemented. M7 (safe writes and
commands) is next, followed by validation gate V3. See the
[M6 evidence and limitations](docs/evidence/M6-reference-worker.md).

The differentiating capability is built and runs end to end against fakes: a task can be **stopped, understood, continued, handed to a different worker, and carried out of the application** as a record that stands on its own.

- A checkpoint is *derived* from the durable events rather than kept beside them, so the two cannot disagree after an unclean stop — and a checkpoint can be taken for a task the running process never started, which is what makes startup recovery possible at all.
- Continuing is always explicit. A step is identified by a worker-declared name plus a fingerprint of the tool and its arguments, and the core refuses a step already in the completed watermark — so *resume without repeating a side effect* does not depend on the worker cooperating. An action whose effects are unsafe or unknown leaves the task `interrupted` for a person to reconcile.
- A second worker with different capabilities can take a task over from canonical state alone. It receives a handoff bundle and nothing else; the one field that could carry the previous worker's private session is recorded as an omission instead of being passed along.
- Export is a data-release action. Secrets and worker-private state are excluded unconditionally and are not options; absolute paths and raw tool output are excluded by default. Everything dropped is counted in the manifest and named in a README the bundle carries. Import treats its input as hostile and executes nothing.

Earlier milestones stand behind it: every tool call passes a fail-closed permission boundary and lands in the record with its origin, capability, scope, and decision, refusals included (M3); projects, conversations, tasks, ordered events, and secret-free configuration manifests persist in SQLite and recover on restart (M2).

M5 added the model path: configured endpoints, a deterministic router, and a release boundary a provider cannot be reached without.

- A model provider takes an **authorized** request, and the only way to obtain one is the release manager. A cloud endpoint cannot be reached without a disclosure and an approval, by construction rather than by discipline — the same technique that makes a tool unreachable without a permission decision.
- What is disclosed is computed from the payload, not from the assembled context, so the release plan cannot describe something other than what was sent.
- Credentials live in the operating system's credential store. Only an opaque reference is persisted, and a value reaches a provider as one header on one request.
- Whether work leaves the device is **derived** — from the endpoint's URL, from the model's location — and never declared, so no settings row can mark a third-party endpoint local and slip past Local Only.
- Credential-shaped text is redacted where it enters the system, so the durable record never held it.

**A real model streams end to end through the CLI. No request has been made to OpenAI or Anthropic**, no test performs a TLS handshake, and no real `llama-server` has been supervised — those are named in [the M5 evidence note](docs/evidence/M5-models-and-routing.md) rather than implied. The desktop reaches the same models through the same composition but has no settings screen yet. Modifying tools arrive in M7. Validation gates V1 and V2 passed.

M6 adds model-selected filesystem/Git reads with schema validation, fresh
per-turn disclosure, durable observations and restart-safe budget reservations.
Its end-to-end test uses synthetic inference with real HTTP, filesystem tools
and SQLite; live-model validation of this loop is still outstanding. Structured
answer text is buffered until redaction, while task activity remains live.
Profiles without enabled endpoints retain the labelled diagnostic fake. An
enabled endpoint selects the real loop and must explicitly advertise tool-call
support; broken configuration never silently becomes fake inference.

```bash
corepack enable pnpm     # Node 24.x; Node 25 removed Corepack
pnpm install
pnpm bootstrap           # validates the toolchain, generates contract artifacts
pnpm check:fast          # format, contracts, lint, types, tests (~5s)
pnpm check               # the above plus the release build
pnpm dev                 # desktop shell
pnpm dev:cli project open /path/to/project
pnpm dev:cli task "Say hello"   # the same task from a terminal
```

Stopping and continuing work, and carrying it elsewhere (`tramontana` is `pnpm dev:cli` in a development checkout):

```bash
tramontana task inspect  <TASK>                          # state, checkpoint, replay safety
tramontana task resume   <TASK> --checkpoint-id <CP>     # the same worker continues
tramontana task handoff  <TASK> --checkpoint-id <CP> --to fake.receiving
tramontana task discard  <TASK>                          # keeps history, undoes nothing
tramontana bundle export  ./out --project-id <PROJECT>
tramontana bundle inspect ./out                          # validates, writes nothing
tramontana bundle import  ./out                          # into a clean profile

tramontana provider add-openai local --base-url http://127.0.0.1:8080 --model llama3
tramontana provider add-anthropic claude --model claude-sonnet-4-5 --credential anthropic.apiKey
tramontana provider list                                 # shows local vs CLOUD, derived not declared
tramontana provider tool-calls local                     # only after verifying this model supports native tool calls
tramontana provider tool-calls local --off               # unknown/unsupported must stay off

tramontana secret set    openai.apiKey < key.txt         # stdin only, never an argument
tramontana secret status openai.apiKey                   # configured or not; never the value
tramontana secret forget openai.apiKey
```

Prerequisites, root commands, and known gaps: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

Work is sequenced so the differentiating capability is proven against fakes *before* model adapters and workers exist — if the thesis is wrong, that is cheaper to learn now. Validation gates `V1`–`V6` sit between stages; each answers one question with evidence before the next stage begins. Each closed milestone leaves an evidence note in [docs/evidence/](docs/evidence/), including the findings that were wrong along the way.

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
crates/persistence/ SQLite implementation of the local operational plane
crates/platform/    Paths, clock, identifiers, profile lease, bundle files
crates/adapters-tools/  Read-only filesystem and Git tools
crates/adapters-model/  Model provider adapters: chat-completions and Anthropic
crates/adapters-local/  Supervised local model runtimes (llama.cpp)
crates/composition/     Configured endpoints turned into live providers
packages/shared/    Generated schemas and TypeScript client types
xtask/              Repository build tooling; not shipped
scripts/            Repository checks that are not tests
tests/fixtures/     Cross-language contract fixtures, generated by xtask
docs/decisions/     Architecture decision records
docs/evidence/      What each milestone proved, and what it did not
.tramontana/        Portable Markdown customization examples
docs/               Product, architecture, security, and delivery contracts
```

Crates for each adapter family are created when their first code lands, not in advance.

Implementation agents must read `AGENTS.md` and the milestone entry in `docs/BUILD_ORDER.md` before making changes. Architecture changes require corresponding documentation and, where specified, a decision record in the same change.

## License

MIT. See `LICENSE`.
