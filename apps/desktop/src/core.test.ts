import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const { fetchCoreInfo } = await import("./core.js");

describe("core IPC client", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({ schemaVersion: 1, appVersion: "0.1.0" });
  });

  it("invokes the command name the Rust shell registers", async () => {
    // The command name is a string on this side and an identifier on the Rust
    // side, so nothing at compile time links them. `tauri::generate_handler!`
    // in apps/desktop/src-tauri/src/lib.rs registers `core_info`; a rename on
    // either side must fail here rather than at runtime as a silent
    // "core unavailable".
    await fetchCoreInfo();
    expect(invoke).toHaveBeenCalledExactlyOnceWith("core_info");
  });

  it("returns the payload the core reported", async () => {
    await expect(fetchCoreInfo()).resolves.toEqual({
      schemaVersion: 1,
      appVersion: "0.1.0",
    });
  });
});
