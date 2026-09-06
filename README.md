# Tramontana

Tramontana is a local-first, cross-platform desktop environment for working with AI models and agents. It aims to provide the approachable chat and project experience of ChatGPT or Claude, the transparent task execution of coding-agent tools, and a provider-neutral runtime that can keep work on the user's computer or deliberately escalate selected requests to cloud models.

## Phase 1

Phase 1 establishes the personal desktop product for macOS, Windows, and Linux:

- a Tauri desktop shell with a React + TypeScript interface;
- Rust core services for orchestration, permissions, persistence, tools, and model routing;
- local-first inference through `llama.cpp` and MLX on Apple Silicon;
- optional OpenAI, Anthropic, Ollama, and OpenAI-compatible adapters behind stable interfaces;
- coding-agent tools with visible activity and explicit approvals;
- MCP-based Apps and Markdown-based skills, agents, rules, and workflows.

Phase 1 has no enterprise server dependency. The default architecture is personal and local, with cloud use optional, visible, policy-controlled, and minimized.

## Status

This repository currently contains the implementation contract and starter structure, not the full application. Start with:

- [Phase 1 specification](docs/PHASE_1_SPEC.md)
- [Architecture and stable interfaces](docs/ARCHITECTURE.md)
- [Security and permissions](docs/SECURITY_AND_PERMISSIONS.md)
- [Configuration conventions](docs/CONFIGURATION.md)
- [Acceptance criteria](docs/PHASE_1_ACCEPTANCE_CRITERIA.md)
- [Roadmap and explicit scope boundaries](docs/ROADMAP.md)
- [Open-source foundations](docs/OPEN_SOURCE_FOUNDATIONS.md)

## Intended repository layout

```text
apps/desktop/       Tauri application and React user interface
crates/core/        Provider-neutral Rust domain and application services
packages/shared/    Generated/shared schemas and TypeScript client types
.tramontana/        Portable Markdown customization examples
docs/               Product, architecture, security, and delivery contracts
```

Implementation agents must read `AGENTS.md` before making changes. Architecture changes require corresponding documentation updates in the same change.

## License

MIT. See `LICENSE`.
