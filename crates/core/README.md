# Core services

Intended home of provider-neutral Rust domain types, ports, and application services.

Start with the portable project/work records, event/task/checkpoint state, normalized schemas, model/agent/compute/tool/context contracts, worker lifecycle, routing policy, permissions, context provenance, registries, export/import, and repository ports. Do not import concrete OpenAI, Anthropic, llama.cpp, MLX, ACP, external-agent, MCP transport, database, UI, or platform SDK types into the core domain.

Every adapter should be replaceable, and proven so by contract tests.
