# Tramontana

Tramontana is AI for work whose data can't leave the building. It is a
desktop application for macOS, Windows and Linux that runs AI models and
agents on your own computer, so people in industries where privacy is
critical — pharma, healthcare, legal, finance and others — can use AI on
confidential documents and systems.

- **The model runs where the data is.** A project marked Local Only never
  reaches a cloud model. When a cloud model is allowed, Tramontana shows
  exactly what would be sent before anything leaves.
- **Nothing happens without approval.** Every file change, command and tool
  call is asked about before it runs, as is the first visit to each website
  and every form the agent would submit.
- **Everything is recorded.** Each task keeps a record of what was read,
  which model answered, what changed and what was approved. The record survives
  a crash, can be resumed, and can be exported in an open format.
- **Models are replaceable.** Use a model on this computer, or Claude or an
  OpenAI-compatible model when policy allows, and hand a task from one to
  another without losing the work.

Tramontana is pre-release and built from source; see [Status](#status) for
what works today and what does not.

## Status

Tramontana is pre-release: there is no installer yet, and it has not yet been
used by anyone outside the project.

**Works today**

- A desktop application and a CLI over one durable record of every task.
- A built-in agent that reads, edits files and runs the project's declared
  commands, each edit and each command approved by you as it happens.
- Models on your own machine through `llama-server` or Apple's
  `mlx_lm.server`, with **Local Only** routing. OpenAI-compatible and
  Anthropic endpoints can be configured; neither vendor has been tried yet.
- Checkpoints, resume without repeating a side effect, and export and import
  of the work.
- Choosing the model a task runs on, and handing a stopped task to another
  model, which continues from the task's record. A cloud model is offered
  only with what would be sent shown first, and a Local Only task is never
  run on, or handed to, a model off this device.
- The project's own instructions (`AGENTS.md`, `CLAUDE.md`,
  `.tramontana/rules`) and skills (`.tramontana/skills`), given to the model
  with credentials removed. Nothing in them grants a permission.
- Tools from MCP servers over stdio or HTTP, including OAuth sign-in, each
  call approved. A remote App is never used under Local Only.
- A built-in browser in a throwaway profile of your installed Chrome, Edge
  or Chromium; the first visit to each site and every form submission are
  approved.
- Chat, PDF and Word attachments, scheduled tasks, local dictation with
  `whisper.cpp`, and "delete all my data".

**In progress:** shared projects hosted on a company's own machine or
server, with desktops paired to it, and private models on rented GPUs.

**What Tramontana does not do:** it does not sandbox the commands you
approve. An approved command runs with your permissions and can read, write
and connect to anything you can. Tramontana checks file operations against
the folders you granted, runs commands as argument lists with a cleared
environment, and stops every process a command started, but it does not
contain them. Local Only covers models, not commands.

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

**2. Open a project.** **Open project** and choose a folder, ideally a Git
repository. To let a task run the project's own commands, such as its
tests, declare them as recipes in `.tramontana/project.md`:

```yaml
---
recipes:
  test: [sh, check.sh]
---
```

Declaring a recipe grants nothing; every run is still put to you.

A task is also given the project's own instructions — its `AGENTS.md`,
`CLAUDE.md` and any `.tramontana/rules` — so what the repository asks for is
what the model works to. They cannot grant it anything.

A `.tramontana/skills/*.md` file is a reusable procedure. It is offered, never
applied by itself: tick it in the composer, or name it with `--skill-id`, and
that one task follows it. Give it `suggest_when: [rust, cargo]` and it
is marked when the goal mentions one of those words — marked, not chosen.

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
pnpm dev:cli task "Read the README and summarize it" --allow-writes --browse \
    --worker builtin.reference@<ENDPOINT>/<MODEL> --skill-id <SKILL>
pnpm dev:cli conversation create "Questions"   # a chat: no project, no files
pnpm dev:cli task "Summarize this" --conversation-id <CHAT> --attach notes.md
pnpm check:fast                 # format, contracts, lint, types, tests
pnpm check                      # the above plus the release build
```

Stopping and continuing work, carrying it elsewhere, and configuring models:

```bash
tramontana task inspect  <TASK>                          # state, checkpoint, replay safety
tramontana task resume   <TASK> --checkpoint-id <CP>     # the same worker continues
tramontana task handoff  <TASK> --checkpoint-id <CP> \
    --to builtin.reference@<ENDPOINT>/<MODEL>           # an unknown name lists them
tramontana task discard  <TASK>                          # keeps history, undoes nothing
tramontana bundle export  ./out --project-id <PROJECT>
tramontana bundle inspect ./out                          # validates, writes nothing
tramontana bundle import  ./out                          # into a clean profile

tramontana provider add-openai local --base-url http://127.0.0.1:8080 --model llama3
tramontana provider add-anthropic claude --model claude-sonnet-4-5 --credential anthropic.apiKey
tramontana provider list                                 # shows local vs CLOUD, derived not declared
tramontana provider tool-calls local                     # only after verifying this model supports native tool calls
tramontana provider tool-calls local --off               # unknown/unsupported must stay off

tramontana app sign-in   <APP>                            # OAuth, for a remote App that asks for it

tramontana secret set    openai.apiKey < key.txt         # stdin only, never an argument
tramontana secret status openai.apiKey                   # configured or not; never the value
tramontana secret forget openai.apiKey
```

## Repository layout

```text
apps/desktop/       Tauri application and React user interface
apps/cli/           Headless development and recovery adapter
apps/server/        The host program for shared projects on an always-on machine
crates/core/        Provider-neutral Rust domain and application services
crates/persistence/ SQLite implementation of the local operational plane
crates/platform/    Paths, clock, identifiers, profile lease, bundle files
crates/adapters-tools/  Filesystem, Git and recipe tools, and the command broker
crates/adapters-mcp/    MCP Apps over stdio or HTTP, behind the tool port
crates/adapters-browser/ The built-in browser: an installed Chrome, over DevTools
crates/adapters-model/  Model provider adapters: chat-completions and Anthropic
crates/adapters-local/  Supervised local model runtimes (llama.cpp and MLX)
crates/adapters-documents/ Text from PDF and Word attachments
crates/adapters-compute/   Rented GPU providers
crates/composition/     Configured endpoints turned into live providers
crates/host/            The service a host serves to paired desktops
crates/test-support/    Helpers shared by end-to-end tests
packages/shared/    Generated schemas and TypeScript client types
xtask/              Repository build tooling; not shipped
scripts/            Repository checks that are not tests
tests/fixtures/     Cross-language contract fixtures, generated by xtask
.tramontana/        Portable Markdown customization examples
```

Contributors and coding agents: read `AGENTS.md` before making changes.

## License

Apache-2.0. See `LICENSE`.
