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

This repository currently contains the implementation contract and starter structure, not the full application. Start with:

- [Phase 1 specification](docs/PHASE_1_SPEC.md)
- [Strategic positioning and build/integrate choices](docs/STRATEGIC_POSITIONING.md)
- [Architecture and stable interfaces](docs/ARCHITECTURE.md)
- [Portable project and work format](docs/PROJECT_FORMAT.md)
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
