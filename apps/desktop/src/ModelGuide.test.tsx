import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

const modelSetupHints = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  modelSetupHints,
}));

const { ModelGuide } = await import("./ModelGuide.js");

describe("ModelGuide", () => {
  it("gives the commands for what is missing, open when no model is set up", async () => {
    modelSetupHints.mockResolvedValue({ models: [], lookedIn: [], platform: "macos" });
    render(<ModelGuide hasEndpoint={false} />);

    expect(await screen.findByText("brew install llama.cpp")).toBeTruthy();
    expect(
      screen.getByText(/curl -L -o ~\/models\/Qwen2\.5-7B-Instruct-Q4_K_M\.gguf/),
    ).toBeTruthy();
    expect(screen.getByText(/downloads nothing itself/)).toBeTruthy();
    expect(
      screen.getByText("Get a model running on this computer").closest("details"),
    ).toHaveAttribute("open");
  });

  it("says what it found, and looks again when asked", async () => {
    const user = userEvent.setup();
    modelSetupHints
      .mockResolvedValueOnce({ models: [], lookedIn: [], platform: "macos" })
      .mockResolvedValue({
        llamaServer: "/opt/homebrew/bin/llama-server",
        models: [{ path: "/Users/maja/models/q.gguf", bytes: 2 * 1024 ** 3 }],
        lookedIn: ["/Users/maja/models"],
        platform: "macos",
      });
    render(<ModelGuide hasEndpoint={true} />);
    expect(screen.getByText("How to add another model").closest("details")).not.toHaveAttribute(
      "open",
    );

    await user.click(await screen.findByRole("button", { name: "Check again" }));
    await waitFor(() => {
      expect(screen.getAllByText("/opt/homebrew/bin/llama-server").length).toBeGreaterThan(0);
    });
    expect(screen.getByText("/Users/maja/models/q.gguf")).toBeTruthy();
    expect(screen.getByText("2.0 GB")).toBeTruthy();
    expect(screen.queryByText("brew install llama.cpp")).toBeNull();
  });

  it("gives Windows its own commands", async () => {
    modelSetupHints.mockResolvedValue({ models: [], lookedIn: [], platform: "windows" });
    render(<ModelGuide hasEndpoint={false} />);
    expect(await screen.findByText("winget install llama.cpp")).toBeTruthy();
  });
});
