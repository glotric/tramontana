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

Tramontana is working toward its first beta. Milestones M0–M8 and validation
gates V1–V3 are closed; each left an evidence note in
[docs/evidence/](docs/evidence/) that says what it proved and what it did not.

- **Done and demonstrated:** a desktop application and a CLI over one durable
  record; a built-in worker that reads, edits files and runs the project's
  declared commands, each edit and each command approved by you as it
  happens; a local model through `llama-server`, with Local Only routing;
  checkpoints, resume without repeating a side effect, and export and import
  of the work. OpenAI-compatible and Anthropic endpoints can be configured
  and pass the same contract tests, but no request has yet been made to
  either vendor.
- **In progress:** gate V4, whether someone other than the author can operate
  the desktop from this README. Its automated checks pass; its
  demonstration has not run.
- **Built:** MCP Apps over stdio (M9). Added from the CLI
  (`tramontana app add`); seen, turned on and off, and their decisions read
  in the desktop's **Apps** view; allowed for a task with **May use Apps**.
- **Built, beyond Beta 1's scope:** a built-in browser (ADR 0047). A task
  allowed **May browse the web** opens pages in a window of your installed
  Chrome, Edge or Chromium, with a throwaway profile of its own. The first
  visit to each site is asked about, and every form it would submit. Tasks
  run up to 40 steps in 30 minutes.
- **Deferred from Beta 1:** an external coding worker (M10, ADR 0038), and
  the rest listed in the [draft release notes](docs/RELEASE_NOTES_BETA_1.md).

**What Tramontana does not do:** it does not sandbox the commands you
approve. An approved command runs with your permissions. Tramontana checks
file operations against the folders you granted and stops every process a
command started, but it does not contain them. The
[execution boundary](docs/EXECUTION_BOUNDARY.md) says exactly what is
enforced and what is only checked.

## Getting started

Tramontana is built from source; there is no installer yet.

**You need** macOS or Linux (Windows builds, but has not been tried by
hand), Git, the Rust toolchain from [rustup](https://rustup.rs), and
Node.js 24 — not 25. On Linux, Tauri's
[system packages](https://v2.tauri.app/start/prerequisites/). For a model on
your own machine, a [`llama-server`](https://github.com/ggml-org/llama.cpp/releases)
executable and a GGUF model file; the one tested is Qwen2.5-7B-Instruct
(Q4_K_M), and a model must make native tool calls to edit files. A cloud
model can be configured instead — an OpenAI-compatible or Anthropic
endpoint and its key — though neither vendor has been tried yet.

```bash
corepack enable pnpm     # once, so the pinned pnpm is on your PATH
pnpm install
pnpm bootstrap           # checks the toolchain
pnpm dev                 # builds and opens the desktop; the first run takes a few minutes
```

The terminal you start it from is the application's log; keep it open. Your
work is kept in a profile in the system's application-data folder, or in
the file `TRAMONTANA_PROFILE` names.

**1. Add a model.** Open **Models and credentials**. For a local model,
choose **llama-server started by Tramontana**, give it a name and the
model's name, and **Choose files and add…** to pick the executable and the
model file. On a Mac with Apple Silicon, **mlx_lm.server started by
Tramontana** works the same way with an `mlx_lm.server` you installed
(`pip install mlx-lm`) and an MLX model folder. Tick **Tool calls verified**
only once you know the model makes
native tool calls. A new endpoint is used the next time Tramontana starts:
quit and run `pnpm dev` again.

**2. Open a project.** **+ Open project** and choose a folder, ideally a Git
repository. To let a task run the project's own commands, such as its
tests, declare them as recipes in `.tramontana/project.md`:

```yaml
---
recipes:
  test: [sh, check.sh]
---
```

Declaring a recipe grants nothing; every run is still put to you. See
[configuration](docs/CONFIGURATION.md) for the rest of that file.

**3. Start a task.** Add a conversation, then write the goal in the
composer. By default a task may only read. Tick **May edit files**, **May
run recipes**, **May use Apps** or **May browse the web** to let the model
ask for more, and choose
where models may run: **Local only** keeps every model request on this
device.

**4. Answer what it asks.** Each edit and each command stops the task until
you answer. The question says what will happen — the file, or the exact
command — and **Allow once** lets that one through. **Allow in this
project** lets later ones like it through until Tramontana quits. Watch the
activity cards: which model ran and where, what was sent to it, each tool
call and how it was decided.

**5. Review the change.** The **Changes** panel shows Git's status and diff
for the project, read again whenever a task ends.

**6. Stop and continue.** **Stop** cancels a task. If Tramontana closes or
crashes while a task runs, the task comes back **Interrupted**, with a panel
showing what was in flight and whether repeating it is safe. **Resume**
continues from there without redoing what already happened; **Discard**
keeps the history and undoes nothing.

**7. Take the work elsewhere.** **Export this task…** or **Export
project…** writes a bundle to a folder you choose, leaving out absolute
paths and raw tool output unless you ask. **Import a bundle…** shows what a
bundle holds before anything is written, and runs none of it.

### From a terminal

The CLI reads and writes the same profile (`tramontana` is `pnpm dev:cli`
in a checkout):

```bash
pnpm dev:cli project open /path/to/project
pnpm dev:cli task "Say hello"   # a read-only task
pnpm check:fast                 # format, contracts, lint, types, tests
pnpm check                      # the above plus the release build
```

Stopping and continuing work, carrying it elsewhere, and configuring models:

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

## Repository layout

```text
apps/desktop/       Tauri application and React user interface
apps/cli/           Headless development and recovery adapter
crates/core/        Provider-neutral Rust domain and application services
crates/persistence/ SQLite implementation of the local operational plane
crates/platform/    Paths, clock, identifiers, profile lease, bundle files
crates/adapters-tools/  Filesystem, Git and recipe tools, and the command broker
crates/adapters-mcp/    MCP Apps over stdio, behind the tool port
crates/adapters-browser/ The built-in browser: an installed Chrome, over DevTools
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
