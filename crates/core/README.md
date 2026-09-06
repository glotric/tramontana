# Core services

Intended home of provider-neutral Rust domain types, ports, and application services.

Start with event/task state, normalized schemas, provider contracts, routing policy, permissions, context provenance, registries, and repository ports. Do not import concrete OpenAI, Anthropic, llama.cpp, MLX, MCP transport, database, UI, or platform SDK types into the core domain.

Every adapter should be replaceable through contract tests documented in `docs/ARCHITECTURE.md`.
