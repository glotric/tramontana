import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConversationRecord, ProjectRecord } from "@tramontana/shared";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const listConversations = vi.hoisted(() => vi.fn());
vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, listConversations };
});

const { Sidebar } = await import("./Sidebar.js");

function project(projectId: string, name: string): ProjectRecord {
  return {
    schemaVersion: 1,
    projectId,
    name,
    roots: [`/work/${projectId}`],
    createdAt: "2026-01-01",
  };
}

function chat(conversationId: string, projectId: string): ConversationRecord {
  return {
    schemaVersion: 1,
    conversationId,
    projectId,
    title: `Chat ${conversationId}`,
    createdAt: "2026-01-01",
  };
}

const alpha = project("A", "Alpha");
const beta = project("B", "Beta");

/** The sidebar in Agent mode with `openProjectId` open. */
function sidebar(openProjectId: string, conversations: readonly ConversationRecord[]) {
  return (
    <Sidebar
      mode="agent"
      view="work"
      onView={() => undefined}
      onHide={() => undefined}
      onSearch={() => undefined}
      onNew={() => undefined}
      chats={[]}
      selectedChatId={undefined}
      onSelectChat={() => undefined}
      projects={[alpha, beta]}
      openProjectId={openProjectId}
      conversations={conversations}
      selectedConversationId={undefined}
      onOpenConversation={() => undefined}
      onNewTaskIn={() => undefined}
      onOpenProject={() => undefined}
      onRemoveProject={() => Promise.resolve()}
      openingProject={false}
      canOpenProject
      personName={undefined}
      status="ready"
      statusText="Ready"
    />
  );
}

function expanded(name: string): string | null {
  return screen.getByRole("button", { name }).getAttribute("aria-expanded");
}

beforeEach(() => {
  listConversations.mockReset();
});

describe("Sidebar", () => {
  it("keeps a project expanded when one of its conversations opens it", async () => {
    // Expanding a project and clicking one of its conversations opens that
    // project. It used to collapse at that moment, hiding the conversation
    // just opened.
    const user = userEvent.setup();
    listConversations.mockResolvedValue([chat("b1", "B")]);
    const view = render(sidebar("A", [chat("a1", "A")]));

    await user.click(screen.getByRole("button", { name: "Beta" }));
    await screen.findByRole("button", { name: "Chat b1" });
    view.rerender(sidebar("B", [chat("b1", "B")]));

    expect(expanded("Beta")).toBe("true");
    expect(screen.getByRole("button", { name: "Chat b1" })).toBeVisible();
    // The project no longer open was never touched, so it folds away.
    expect(expanded("Alpha")).toBe("false");
  });

  it("keeps a collapsed project collapsed whichever project is open", async () => {
    const user = userEvent.setup();
    const view = render(sidebar("A", [chat("a1", "A")]));

    await user.click(screen.getByRole("button", { name: "Alpha" }));
    expect(expanded("Alpha")).toBe("false");
    view.rerender(sidebar("B", []));
    expect(expanded("Alpha")).toBe("false");
  });

  it("reads a project's conversations again once it is no longer the open one", async () => {
    // Its list came from the window while it was open; what was shown when
    // it was first expanded is stale by then.
    const user = userEvent.setup();
    listConversations.mockResolvedValue([chat("b1", "B")]);
    const view = render(sidebar("A", []));
    await user.click(screen.getByRole("button", { name: "Beta" }));
    await screen.findByRole("button", { name: "Chat b1" });

    view.rerender(sidebar("B", [chat("b2", "B"), chat("b1", "B")]));
    listConversations.mockResolvedValue([chat("b2", "B"), chat("b1", "B")]);
    view.rerender(sidebar("A", []));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Chat b2" })).toBeVisible();
    });
    expect(listConversations).toHaveBeenLastCalledWith("B");
  });
});
