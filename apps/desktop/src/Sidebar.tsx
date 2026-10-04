import { useState } from "react";
import type { ConversationRecord, ProjectRecord } from "@tramontana/shared";

import { listConversations } from "./core.js";
import { Glyph } from "./Glyph.js";
import { Icon } from "./icons.js";
import type { IconName } from "./icons.js";

export type Mode = "chat" | "agent";
export type View = "work" | "models" | "apps" | "projects" | "settings" | "scheduled";

/** Whether "More" was last left open: a convenience of this window only. */
const MORE_KEY = "tramontana.sidebar.more";

function rememberedMore(): boolean {
  try {
    return localStorage.getItem(MORE_KEY) === "open";
  } catch {
    return false;
  }
}

/** The search shortcut as this platform writes it. */
const SHORTCUT =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘K" : "Ctrl K";

function NavRow({
  icon,
  label,
  pressed,
  onClick,
  disabled,
}: {
  readonly icon: IconName;
  readonly label: string;
  readonly pressed?: boolean;
  readonly onClick: () => void;
  readonly disabled?: boolean;
}): React.JSX.Element {
  return (
    <button
      type="button"
      className="rail-row rail-row--nav"
      aria-pressed={pressed}
      onClick={onClick}
      disabled={disabled}
    >
      <Icon name={icon} />
      <span>{label}</span>
    </button>
  );
}

/**
 * The sidebar, the same in Chat and Agent: search, a new chat or task,
 * schedules, the rarer screens under More, then the mode's own list, and
 * the person and the core's state at the foot.
 */
export function Sidebar({
  mode,
  view,
  onView,
  onHide,
  onSearch,
  onNew,
  chats,
  selectedChatId,
  onSelectChat,
  projects,
  openProjectId,
  conversations,
  selectedConversationId,
  onOpenConversation,
  onNewTaskIn,
  onOpenProject,
  openingProject,
  canOpenProject,
  personName,
  status,
  statusText,
}: {
  readonly mode: Mode;
  readonly view: View;
  /** Show a screen; the same screen again goes back to work. */
  readonly onView: (view: View) => void;
  readonly onHide: () => void;
  readonly onSearch: () => void;
  readonly onNew: () => void;
  readonly chats: readonly ConversationRecord[];
  readonly selectedChatId: string | undefined;
  readonly onSelectChat: (conversationId: string) => void;
  readonly projects: readonly ProjectRecord[];
  readonly openProjectId: string | undefined;
  /** The open project's conversations, kept current by the window. */
  readonly conversations: readonly ConversationRecord[];
  readonly selectedConversationId: string | undefined;
  readonly onOpenConversation: (projectId: string, conversationId: string) => void;
  readonly onNewTaskIn: (projectId: string) => void;
  readonly onOpenProject: () => void;
  readonly openingProject: boolean;
  readonly canOpenProject: boolean;
  readonly personName: string | undefined;
  readonly status: string;
  readonly statusText: string;
}): React.JSX.Element {
  const [moreOpen, setMoreOpen] = useState(rememberedMore);
  // Projects the person opened or closed against the default, which is
  // open for the project on screen and closed for the rest.
  const [toggled, setToggled] = useState<ReadonlySet<string>>(new Set());
  // Conversations of the other expanded projects, read when expanded.
  const [listed, setListed] = useState<ReadonlyMap<string, readonly ConversationRecord[]>>(
    new Map(),
  );

  const isExpanded = (projectId: string): boolean =>
    (projectId === openProjectId) !== toggled.has(projectId);

  const toggleMore = (): void => {
    setMoreOpen((current) => {
      try {
        localStorage.setItem(MORE_KEY, current ? "closed" : "open");
      } catch {
        // A window that cannot remember still opens and closes.
      }
      return !current;
    });
  };

  const toggleProject = (projectId: string): void => {
    const opening = !isExpanded(projectId);
    setToggled((current) => {
      const next = new Set(current);
      if (next.has(projectId)) next.delete(projectId);
      else next.add(projectId);
      return next;
    });
    if (opening && projectId !== openProjectId) {
      void listConversations(projectId).then(
        (found) => {
          setListed((current) => new Map(current).set(projectId, found));
        },
        () => undefined,
      );
    }
  };

  const conversationsOf = (projectId: string): readonly ConversationRecord[] =>
    projectId === openProjectId ? conversations : (listed.get(projectId) ?? []);

  return (
    <aside className="rail" aria-label="Projects and conversations">
      <div className="brand-lockup">
        <Glyph icon="wind" hue="blue" size={30} />
        <strong>Tramontana</strong>
        <button type="button" className="icon-button" aria-label="Hide sidebar" onClick={onHide}>
          <Icon name="sidebar" />
        </button>
      </div>

      <button type="button" className="rail-search" aria-haspopup="dialog" onClick={onSearch}>
        <Icon name="search" size={16} />
        <span className="rail-row-label">Search</span>
        <kbd className="rail-shortcut" aria-hidden="true">
          {SHORTCUT}
        </kbd>
      </button>

      <NavRow
        icon="plus"
        label={mode === "chat" ? "New chat" : "New task"}
        pressed={
          view === "work" &&
          (mode === "chat" ? selectedChatId === undefined : selectedConversationId === undefined)
        }
        onClick={onNew}
      />
      <NavRow
        icon="clock"
        label="Scheduled"
        pressed={view === "scheduled"}
        onClick={() => {
          onView("scheduled");
        }}
      />
      <button
        type="button"
        className="rail-row rail-row--nav rail-more"
        aria-expanded={moreOpen}
        onClick={toggleMore}
      >
        <Icon name={moreOpen ? "chevronDown" : "chevronRight"} />
        <span>More</span>
      </button>
      {moreOpen && (
        <div className="rail-more-list">
          <NavRow
            icon="folder"
            label="Projects"
            pressed={view === "projects"}
            onClick={() => {
              onView("projects");
            }}
          />
          <NavRow
            icon="cpu"
            label="Models and credentials"
            pressed={view === "models"}
            onClick={() => {
              onView("models");
            }}
          />
          <NavRow
            icon="apps"
            label="Apps"
            pressed={view === "apps"}
            onClick={() => {
              onView("apps");
            }}
          />
        </div>
      )}

      <nav className="rail-scroll" aria-label="Recent work">
        {mode === "chat" ? (
          <section>
            <h2 className="rail-heading">Chats</h2>
            {chats.length === 0 ? (
              <p className="rail-empty">No chats yet.</p>
            ) : (
              <ul className="rail-list">
                {chats.map((chat) => (
                  <li key={chat.conversationId}>
                    <button
                      type="button"
                      className="rail-row"
                      data-selected={selectedChatId === chat.conversationId}
                      aria-current={selectedChatId === chat.conversationId ? "true" : undefined}
                      onClick={() => {
                        onSelectChat(chat.conversationId);
                      }}
                    >
                      <span>{chat.title}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        ) : (
          <section>
            <div className="rail-heading-row">
              <h2 className="rail-heading">Projects</h2>
              <button
                type="button"
                className="rail-add"
                aria-label={openingProject ? "Opening a project" : "Open project"}
                title="Open a folder as a project"
                onClick={onOpenProject}
                disabled={!canOpenProject || openingProject}
              >
                <Icon name="plus" size={16} />
              </button>
            </div>
            {projects.length === 0 ? (
              <p className="rail-empty">No projects yet. Open a folder to start.</p>
            ) : (
              <ul className="rail-list">
                {projects.map((project) => {
                  const open = isExpanded(project.projectId);
                  const items = conversationsOf(project.projectId);
                  return (
                    <li key={project.projectId} className="rail-project">
                      <div className="rail-project-row">
                        <button
                          type="button"
                          className="rail-row rail-row--project"
                          aria-expanded={open}
                          aria-current={openProjectId === project.projectId ? "true" : undefined}
                          data-selected={openProjectId === project.projectId}
                          onClick={() => {
                            toggleProject(project.projectId);
                          }}
                        >
                          <span>{project.name}</span>
                          <Icon name={open ? "chevronDown" : "chevronRight"} size={14} />
                        </button>
                        <button
                          type="button"
                          className="rail-add rail-add--hover"
                          aria-label={`New task in ${project.name}`}
                          onClick={() => {
                            onNewTaskIn(project.projectId);
                          }}
                        >
                          <Icon name="plus" size={16} />
                        </button>
                      </div>
                      {open &&
                        (items.length === 0 ? (
                          <p className="rail-empty rail-empty--nested">No conversations yet.</p>
                        ) : (
                          <ul className="rail-list rail-list--nested">
                            {items.map((conversation) => {
                              const selected =
                                selectedConversationId === conversation.conversationId;
                              return (
                                <li key={conversation.conversationId}>
                                  <button
                                    type="button"
                                    className="rail-row rail-row--conversation"
                                    data-selected={selected}
                                    aria-current={selected ? "true" : undefined}
                                    onClick={() => {
                                      onOpenConversation(
                                        project.projectId,
                                        conversation.conversationId,
                                      );
                                    }}
                                  >
                                    <span>{conversation.title}</span>
                                  </button>
                                </li>
                              );
                            })}
                          </ul>
                        ))}
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        )}
      </nav>

      <button
        type="button"
        className="rail-row rail-person"
        aria-pressed={view === "settings"}
        onClick={() => {
          onView("settings");
        }}
      >
        <span className="rail-avatar" aria-hidden="true">
          {(personName?.trim()[0] ?? "").toUpperCase() || <Icon name="gear" size={15} />}
        </span>
        <span className="rail-person-name">
          {personName === undefined || personName.trim() === "" ? (
            "Settings"
          ) : (
            <>
              {personName}
              <span className="rail-person-sub"> · Settings</span>
            </>
          )}
        </span>
        <Icon name="gear" size={16} />
      </button>
      <footer className="rail-status">
        <span className="status-dot" data-state={status} aria-hidden="true" />
        <span>{statusText}</span>
      </footer>
    </aside>
  );
}
