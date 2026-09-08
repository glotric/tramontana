# Contract fixtures, schema version 1

Real event streams produced by `tramontana task --json` against the M1 fake
providers. Regenerate with:

```
cargo run -p tramontana-cli -- task "Say hello" --chunk-delay-ms 0 --json > task-completed.ndjson
cargo run -p tramontana-cli -- task "Say hello" --chunk-delay-ms 40 --cancel-after-ms 50 --json > task-cancelled.ndjson
```

Deterministic: the fake clock and identifier source make every run byte-identical
for `task-completed.ndjson`. `task-cancelled.ndjson` depends on a timer, so the
exact sequence at which cancellation lands may vary; tests assert on its shape
and terminal state, not its length.

These are the shared cross-language fixtures required by `TESTING_STRATEGY.md`
§4: Rust writes them, TypeScript parses and validates them against the committed
JSON Schema. Every value is synthetic.
