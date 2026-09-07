import { invoke } from "@tauri-apps/api/core";
import type { CoreInfo } from "@tramontana/shared";

/**
 * Thin handwritten client over the Tauri command boundary.
 *
 * Everything here is transport: naming, shape, and error normalization. No
 * policy, no domain rules (`docs/CORE_CONTRACTS.md` §14).
 */

/** Wire shape of the `core_info` response, before camelCase normalization. */
interface CoreInfoPayload {
  readonly schemaVersion: number;
  readonly appVersion: string;
}

/** Ask the running core for its contract schema and application version. */
export async function fetchCoreInfo(): Promise<CoreInfo> {
  const payload = await invoke<CoreInfoPayload>("core_info");
  return { schemaVersion: payload.schemaVersion, appVersion: payload.appVersion };
}
