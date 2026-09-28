import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConversationRecord } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const previewTranscript = vi.hoisted(() => vi.fn());
const saveTranscript = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, previewTranscript, saveTranscript };
});

const { ShareDialog } = await import("./ShareDialog.js");

const chat: ConversationRecord = {
  schemaVersion: 1,
  conversationId: "chat-1",
  title: "Winds",
  createdAt: "2026-09-28T00:00:00.000Z",
};

beforeEach(() => {
  previewTranscript.mockReset();
  saveTranscript.mockReset();
  previewTranscript.mockImplementation(
    (_id: string, options: { format: string; includeAttachments: boolean }) =>
      Promise.resolve(
        `${options.format}:${options.includeAttachments ? "with files" : "names only"}`,
      ),
  );
});

describe("ShareDialog", () => {
  it("shows exactly what will be saved, for the chosen format and files", async () => {
    const user = userEvent.setup();
    render(<ShareDialog conversation={chat} onClose={vi.fn()} />);

    const preview = screen.getByRole("region", { name: "What will be saved" });
    await waitFor(() => {
      expect(preview).toHaveTextContent("markdown:names only");
    });
    await user.click(screen.getByRole("radio", { name: "Web page" }));
    await user.click(screen.getByRole("checkbox", { name: "Include attached files' text" }));
    await waitFor(() => {
      expect(preview).toHaveTextContent("html:with files");
    });
  });

  it("saves with the same choices, and says where", async () => {
    const user = userEvent.setup();
    saveTranscript.mockResolvedValue("/Users/me/Winds.md");
    render(<ShareDialog conversation={chat} onClose={vi.fn()} />);
    await screen.findByText("markdown:names only");

    await user.click(screen.getByRole("button", { name: "Save as file…" }));
    expect(saveTranscript).toHaveBeenCalledWith("chat-1", {
      format: "markdown",
      includeAttachments: false,
    });
    expect(await screen.findByRole("status")).toHaveTextContent("Saved to /Users/me/Winds.md");
  });

  it("writes nothing when the person cancels the save dialog", async () => {
    const user = userEvent.setup();
    saveTranscript.mockResolvedValue(undefined);
    render(<ShareDialog conversation={chat} onClose={vi.fn()} />);
    await screen.findByText("markdown:names only");
    await user.click(screen.getByRole("button", { name: "Save as file…" }));
    await waitFor(() => {
      expect(saveTranscript).toHaveBeenCalled();
    });
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
