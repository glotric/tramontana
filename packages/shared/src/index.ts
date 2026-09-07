/**
 * Versioned types for the Rust-to-interface boundary.
 *
 * This package mirrors the core contract; it owns no domain rules and must
 * never re-implement routing, permission, or redaction policy in the
 * presentation layer (`docs/ARCHITECTURE.md` §2). Contract types are generated
 * from `crates/core/src/contracts` — see `./generated/contracts.js`.
 */

export * from "./generated/contracts.js";

/**
 * Version of the contract schemas this build speaks.
 *
 * Mirrors `tramontana_core::SCHEMA_VERSION`. The two are kept honest by
 * `schemaVersionFromSchema`, which reads the generated schema document.
 */
export const SCHEMA_VERSION = 1;

/** Payload returned by the `core_info` command. */
export interface CoreInfo {
  /** Contract schema version reported by the running core. */
  readonly schemaVersion: number;
  /** Version of the desktop shell. */
  readonly appVersion: string;
}

/**
 * Whether a core reporting `schemaVersion` can be understood by this build.
 *
 * Consumers of the event stream must additionally tolerate unknown future
 * event kinds within a compatible version (`docs/CORE_CONTRACTS.md` §2).
 */
export function isSchemaCompatible(schemaVersion: number): boolean {
  return schemaVersion === SCHEMA_VERSION;
}
