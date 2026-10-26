import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppView } from "./core.js";

const listApps = vi.hoisted(() => vi.fn());
const setAppEnabled = vi.hoisted(() => vi.fn());
const appPermissionHistory = vi.hoisted(() => vi.fn());
const restartSupported = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  listApps,
  setAppEnabled,
  appPermissionHistory,
  restartSupported,
}));

const { AppsView } = await import("./AppsView.js");

const running: AppView = {
  appId: "fixture",
  displayName: "Fixture",
  enabled: true,
  status: { kind: "running" },
  program: "fake-mcp-server",
  variables: ["GREETING_STYLE"],
  secrets: ["TOKEN"],
  protocolVersion: "2025-11-25",
  tools: [
    { toolId: "mcp.fixture.greeting", description: "Says hello.", offered: true },
    { toolId: "mcp.fixture.stamp", description: "Claims nothing.", offered: false },
  ],
  refused: [{ name: "../escape", reason: "its name is not a safe identifier" }],
  diagnostics: "warning: slow start",
};

describe("the Apps screen", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listApps.mockResolvedValue([running]);
    setAppEnabled.mockResolvedValue(undefined);
    restartSupported.mockResolvedValue(false);
    appPermissionHistory.mockResolvedValue({ decisions: [], partial: false });
  });

  it("says how an App is doing, what it offers, and marks its claims as its own", async () => {
    render(<AppsView onClose={() => undefined} />);
    const app = within(await screen.findByRole("list", { name: "Configured Apps" }));

    expect(app.getByText("Running")).toBeInTheDocument();
    expect(app.getByText(/fake-mcp-server/)).toBeInTheDocument();
    const tools = within(app.getByRole("list", { name: "Tools of Fixture" })).getAllByRole(
      "listitem",
    );
    expect(tools[0]).toHaveTextContent("may offer it. Not verified.");
    expect(tools[1]).toHaveTextContent("never offered to a model");
    expect(app.getByRole("list", { name: "Tools refused from Fixture" })).toHaveTextContent(
      "../escape: its name is not a safe identifier",
    );
  });

  it("says where a remote App is reached, rather than a program", async () => {
    listApps.mockResolvedValue([
      {
        ...running,
        program: "",
        address: "https://mcp.example.com/mcp",
        variables: [],
        secrets: [],
      },
    ]);
    render(<AppsView onClose={() => undefined} />);
    const app = within(await screen.findByRole("list", { name: "Configured Apps" }));
    expect(app.getByText("https://mcp.example.com/mcp")).toBeInTheDocument();
    expect(app.queryByText(/runs/)).not.toBeInTheDocument();
  });

  it("names what an App is given, never the values", async () => {
    render(<AppsView onClose={() => undefined} />);
    expect(
      await screen.findByText(/Given GREETING_STYLE, TOKEN \(secret\); values are not shown\./),
    ).toBeInTheDocument();
  });

  it("puts the diagnostics where a keyboard can reach and scroll them", async () => {
    const user = userEvent.setup();
    render(<AppsView onClose={() => undefined} />);
    const diagnostics = await screen.findByRole("region", {
      name: "What Fixture last wrote to its error stream",
    });
    expect(diagnostics).toHaveTextContent("warning: slow start");
    diagnostics.focus();
    await user.tab({ shift: true });
    await user.tab();
    expect(diagnostics).toHaveFocus();
  });

  it("turns an App off by its identifier and says it takes effect on restart", async () => {
    const user = userEvent.setup();
    render(<AppsView onClose={() => undefined} />);
    await user.click(await screen.findByRole("checkbox", { name: "Enabled" }));

    await waitFor(() => {
      expect(setAppEnabled).toHaveBeenCalledWith("fixture", false);
    });
    expect(await screen.findByText(/take effect once Tramontana restarts/)).toBeInTheDocument();
    expect(screen.getByText(/cannot restart itself/)).toBeInTheDocument();
    expect(listApps).toHaveBeenCalledTimes(2);
  });

  it("says each status in words", async () => {
    listApps.mockResolvedValue([
      {
        ...running,
        appId: "a",
        displayName: "A",
        status: { kind: "stopped", reason: "it exited" },
      },
      {
        ...running,
        appId: "b",
        displayName: "B",
        status: { kind: "didNotStart", reason: "no such program" },
      },
      { ...running, appId: "c", displayName: "C", enabled: false, status: { kind: "off" } },
      { ...running, appId: "d", displayName: "D", status: { kind: "startsOnRestart" } },
      {
        ...running,
        appId: "e",
        displayName: "E",
        enabled: false,
        status: { kind: "stopsOnRestart" },
      },
    ]);
    render(<AppsView onClose={() => undefined} />);

    expect(await screen.findByText("Stopped: it exited")).toBeInTheDocument();
    expect(screen.getByText("Did not start: no such program")).toBeInTheDocument();
    expect(screen.getByText("Turned off")).toBeInTheDocument();
    expect(screen.getByText("Turned on; starts when Tramontana restarts")).toBeInTheDocument();
    expect(
      screen.getByText("Turned off; still running until Tramontana restarts"),
    ).toBeInTheDocument();
  });

  it("shows what was decided about an App's tools when asked, and says when it stopped looking", async () => {
    appPermissionHistory.mockResolvedValue({
      decisions: [
        {
          taskId: "task-2",
          goal: "Greet Ada",
          toolId: "mcp.fixture.greeting",
          allowed: true,
          reasonCode: "permission.allowOnce",
          grantKind: "once",
          outcome: "completed",
          decidedAt: "2026-09-21T10:00:00.000Z",
        },
        {
          taskId: "task-1",
          goal: "Greet Bob",
          toolId: "mcp.fixture.greeting",
          allowed: false,
          reasonCode: "permission.denied",
          decidedAt: "2026-09-20T10:00:00.000Z",
        },
      ],
      partial: true,
    });
    const user = userEvent.setup();
    render(<AppsView onClose={() => undefined} />);

    // Read only when asked: a profile's record can be large.
    const show = await screen.findByRole("button", { name: "Show decisions" });
    expect(appPermissionHistory).not.toHaveBeenCalled();
    await user.click(show);
    const decisions = within(
      await screen.findByRole("list", { name: "Decisions about fixture" }),
    ).getAllByRole("listitem");
    expect(appPermissionHistory).toHaveBeenCalledWith("fixture");
    expect(decisions[0]).toHaveTextContent("allowed once · completed");
    expect(decisions[0]).toHaveTextContent("Greet Ada");
    expect(decisions[1]).toHaveTextContent("refused (permission.denied)");
    expect(screen.getByRole("note")).toHaveTextContent("older tasks were not searched");
  });

  it("says when there is nothing to show, and when the settings cannot be read", async () => {
    listApps.mockResolvedValueOnce([]);
    const { unmount } = render(<AppsView onClose={() => undefined} />);
    expect(await screen.findByText(/No Apps are configured/)).toBeInTheDocument();
    unmount();

    listApps.mockRejectedValueOnce({
      code: "persistenceError",
      message: "could not read the Apps: locked",
      retryable: true,
    });
    render(<AppsView onClose={() => undefined} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not read the Apps: locked");
  });
});
