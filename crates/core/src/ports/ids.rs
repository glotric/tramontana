/// The source of new identifiers.
///
/// Injected rather than generated inline so that a test run produces a stable,
/// readable event stream. `prefix` is a human-facing hint only — nothing may
/// derive meaning or authority from an identifier's shape
/// (`CORE_CONTRACTS.md` §2).
pub trait IdSource: Send + Sync {
    /// Produce a new globally unique identifier.
    fn next(&self, prefix: &str) -> String;
}
