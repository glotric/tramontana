import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const fetchCoreInfo = vi.hoisted(() => vi.fn());
vi.mock("./core.js", () => ({ fetchCoreInfo }));

const { App } = await import("./App.js");

describe("M0 shell", () => {
  beforeEach(() => {
    fetchCoreInfo.mockReset();
    fetchCoreInfo.mockResolvedValue({ schemaVersion: 1, appVersion: "0.1.0" });
  });

  it("reports a compatible core once the handshake resolves", async () => {
    render(<App />);
    expect(await screen.findByRole("status")).toHaveTextContent(/core 0\.1\.0/i);
  });

  it("reports a core whose contract version this build does not speak", async () => {
    fetchCoreInfo.mockResolvedValue({ schemaVersion: 99, appVersion: "0.1.0" });
    render(<App />);
    expect(await screen.findByRole("status")).toHaveTextContent(/speaks contracts v99/i);
  });

  it("stays usable when the core is unreachable outside the desktop shell", async () => {
    fetchCoreInfo.mockRejectedValue(new Error("no ipc"));
    render(<App />);
    expect(await screen.findByRole("status")).toHaveTextContent(/core unavailable/i);
  });
});
