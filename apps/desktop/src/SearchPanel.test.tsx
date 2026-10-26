import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ConversationRecord, SearchResults } from "@tramontana/shared";

const search = vi.hoisted(() => vi.fn());
const renameConversation = vi.hoisted(() => vi.fn());
const deleteConversation = vi.hoisted(() => vi.fn());

vi.mock(import("./core.js"), async (importOriginal) => ({
  ...(await importOriginal()),
  search,
  renameConversation,
  deleteConversation,
}));

const { SearchPanel } = await import("./SearchPanel.js");
const { ConversationMenu } = await import("./ConversationMenu.js");

const chat: ConversationRecord = {
  schemaVersion: 1,
  conversationId: "chat-1",
  title: "Bora warnings",
  createdAt: "2026-09-28T00:00:00.000Z",
};

const found: SearchResults = {
  schemaVersion: 1,
  conversations: [
    { conversation: chat, matchedIn: "title" },
    {
      conversation: { ...chat, conversationId: "c2", title: "Misc", projectId: "p1" },
      projectName: "Harbour cranes",
      matchedIn: "goal",
      excerpt: "When does the bora blow?",
    },
  ],
  projects: [
    {
      schemaVersion: 1,
      projectId: "p1",
      name: "Harbour cranes",
      roots: ["/work/cranes"],
      createdAt: "2026-09-28T00:00:00.000Z",
    },
  ],
};

describe("SearchPanel", () => {
  it("searches as the person types, and opens what they pick", async () => {
    const user = userEvent.setup();
    search.mockResolvedValue(found);
    const onOpenConversation = vi.fn();
    const onOpenProject = vi.fn();
    render(
      <SearchPanel
        onClose={vi.fn()}
        onOpenConversation={onOpenConversation}
        onOpenProject={onOpenProject}
      />,
    );

    const field = screen.getByLabelText("Search chats and projects");
    expect(field).toHaveFocus();
    await user.type(field, "bora");
    await waitFor(() => {
      expect(search).toHaveBeenLastCalledWith("bora");
    });
    expect(
      await screen.findByRole("button", { name: /Misc\s*Harbour cranes · Asked: When does/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Bora warnings\s*Chat · Title/ }));
    expect(onOpenConversation).toHaveBeenCalledWith(found.conversations[0]);
    await user.click(screen.getByRole("button", { name: /Harbour cranes\s*\/work\/cranes/ }));
    expect(onOpenProject).toHaveBeenCalledWith(found.projects[0]);
  });

  it("says when nothing matches, and closes on Escape", async () => {
    const user = userEvent.setup();
    search.mockResolvedValue({ schemaVersion: 1, conversations: [], projects: [] });
    const onClose = vi.fn();
    render(<SearchPanel onClose={onClose} onOpenConversation={vi.fn()} onOpenProject={vi.fn()} />);
    await user.type(screen.getByLabelText("Search chats and projects"), "zzz");
    expect(await screen.findByText("Nothing matches “zzz”.")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });
});

describe("ConversationMenu", () => {
  it("renames the conversation", async () => {
    const user = userEvent.setup();
    const onRenamed = vi.fn();
    renameConversation.mockResolvedValue({ ...chat, title: "Adriatic winds" });
    render(<ConversationMenu conversation={chat} onRenamed={onRenamed} onDeleted={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Conversation options" }));
    await user.click(screen.getByRole("button", { name: "Rename" }));
    const title = screen.getByLabelText("Title");
    await user.clear(title);
    await user.type(title, "Adriatic winds{Enter}");

    await waitFor(() => {
      expect(onRenamed).toHaveBeenCalledWith({ ...chat, title: "Adriatic winds" });
    });
    expect(renameConversation).toHaveBeenCalledWith("chat-1", "Adriatic winds");
  });

  it("asks before deleting, and deletes nothing when cancelled", async () => {
    const user = userEvent.setup();
    const onDeleted = vi.fn();
    deleteConversation.mockResolvedValue(undefined);
    render(<ConversationMenu conversation={chat} onRenamed={vi.fn()} onDeleted={onDeleted} />);

    await user.click(screen.getByRole("button", { name: "Conversation options" }));
    await user.click(screen.getByRole("button", { name: "Delete" }));
    expect(screen.getByRole("alertdialog")).toHaveTextContent("This can't be undone.");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(deleteConversation).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Conversation options" }));
    await user.click(screen.getByRole("button", { name: "Delete" }));
    await user.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => {
      expect(onDeleted).toHaveBeenCalledWith("chat-1");
    });
  });

  it("says why a delete was refused", async () => {
    const user = userEvent.setup();
    deleteConversation.mockRejectedValue({
      schemaVersion: 1,
      code: "conflict",
      message: "Stop the conversation's running task before deleting it",
      retryable: true,
      correlationId: "c",
    });
    render(<ConversationMenu conversation={chat} onRenamed={vi.fn()} onDeleted={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Conversation options" }));
    await user.click(screen.getByRole("button", { name: "Delete" }));
    await user.click(screen.getByRole("button", { name: "Delete" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Stop the conversation's running");
  });
});
