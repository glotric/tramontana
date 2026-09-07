/**
 * DO NOT EDIT. Generated from crates/core/src/contracts by `pnpm contracts:generate`.
 *
 * The Rust definitions are the source of truth; `pnpm contracts:check` fails
 * when this file is stale.
 */

/**
 * Sensitivity of a piece of content, used to decide what may leave the device.
 */
export type DataClass = "public" | "project" | "sensitive" | "secret";

/**
 * Where work ran.
 *
 * Recorded on routing decisions and shown to the user, so "did this leave my
 * machine?" is answerable from the durable record alone.
 */
export type ExecutionLocation =
  | {
      /**
       * Human-readable name for the interface.
       */
      displayName: string;
      kind: "local";
      /**
       * Stable identifier for the compute target.
       */
      targetId: string;
    }
  | {
      kind: "cloud";
      /**
       * Stable provider identifier.
       */
      providerId: string;
      /**
       * Provider region, when the provider exposes one.
       */
      region?: string;
    };

/**
 * How much damage a tool invocation could do if the model asked for it wrongly.
 */
export type RiskClass = "read" | "modify" | "dangerous";

/**
 * The user's standing policy for where inference may run.
 */
export type RoutingMode = "localOnly" | "automatic" | "cloudPreferred";

/**
 * An opaque handle to a credential.
 *
 * This is the only representation of a secret that may be persisted, logged,
 * exported, or sent over IPC. `key` is an application-generated identifier
 * and never carries any part of the value it refers to
 * (`SECURITY_AND_PERMISSIONS.md` §7).
 */
export interface SecretRef {
  /**
   * Opaque application-generated key. Never the secret.
   */
  key: string;
  /**
   * Which store resolves this reference.
   */
  store: "platform";
}

/**
 * Where a resolvable secret lives.
 *
 * Only `Platform` exists today. It is an enum rather than a bare marker so
 * that adding a store later is an additive schema change.
 */
export type SecretStore = "platform";

/**
 * What an invocation actually changed, recorded alongside its result.
 */
export type SideEffect = "none" | "localReversible" | "localIrreversible" | "external";

/**
 * Who an event is for.
 *
 * Redaction happens in core before delivery, so `Secret` events are internal
 * audit markers holding references and hashes only, and never reach a webview
 * (`CORE_CONTRACTS.md` §5).
 */
export type Visibility = "user" | "diagnostic" | "secret";
