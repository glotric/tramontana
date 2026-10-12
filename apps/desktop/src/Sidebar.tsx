import { useCallback, useEffect, useRef, useState } from "react";
import type { ConversationRecord, ProjectRecord } from "@tramontana/shared";

import { asErrorEnvelope, listConversations } from "./core.js";
import { Glyph } from "./Glyph.js";
import { Icon } from "./icons.js";
import type { IconName } from "./icons.js";

export type Mode = "chat" | "agent";
export type View = "work" | "projects" | "settings" | "scheduled";

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

/** Where a project's menu opened, and for which project. */
interface MenuAt {
  readonly project: ProjectRecord;
  readonly x: number;
  readonly y: number;
}

/**
 * A project's right-click menu. Opened with the pointer, or from the keyboard
 * with the context-menu key or Shift+F10 on the project's row.
 */
function ProjectMenu({
  at,
  onNewTask,
  onRemove,
  onClose,
}: {
  readonly at: MenuAt;
  readonly onNewTask: () => void;
  readonly onRemove: () => void;
  readonly onClose: () => void;
}): React.JSX.Element {
  const menu = useRef<HTMLDivElement>(null);

  useEffect(() => {
    menu.current?.querySelector("button")?.focus();
    const onPointer = (event: MouseEvent): void => {
      if (menu.current !== null && !menu.current.contains(event.target as Node)) onClose();
    };
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    window.addEventListener("blur", onClose);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);

  return (
    <div
      ref={menu}
      className="context-menu"
      role="menu"
      aria-label={`${at.project.name} options`}
      style={{ left: at.x, top: at.y }}
    >
      <button type="button" role="menuitem" onClick={onNewTask}>
        <Icon name="plus" size={15} />
        New task
      </button>
      <button type="button" role="menuitem" className="context-menu-danger" onClick={onRemove}>
        <Icon name="close" size={15} />
        Remove from Tramontana…
      </button>
    </div>
  );
}

/** Asked before a project is removed: what goes, and that the files stay. */
function RemoveProjectDialog({
  project,
  onConfirm,
  onCancel,
}: {
  readonly project: ProjectRecord;
  readonly onConfirm: () => Promise<void>;
  readonly onCancel: () => void;
}): React.JSX.Element {
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | undefined>();
  const keep = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    keep.current?.focus();
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
    };
  }, [onCancel]);

  return (
    <div className="search-backdrop">
      <div
        className="dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="remove-project-title"
        aria-describedby="remove-project-text"
      >
        <h2 id="remove-project-title">Remove {project.name} from Tramontana?</h2>
        <p id="remove-project-text">
          Its conversations, tasks and records are deleted from Tramontana. The folder and its files
          are not touched: <code>{project.roots[0]}</code>
        </p>
        {failure !== undefined && (
          <p className="notice notice--error" role="alert">
            {failure}
          </p>
        )}
        <div className="dialog-actions">
          <button
            ref={keep}
            type="button"
            className="button button--secondary"
            onClick={onCancel}
            disabled={busy}
          >
            Keep
          </button>
          <button
            type="button"
            className="button button--danger"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              setFailure(undefined);
              onConfirm().catch((error: unknown) => {
                setBusy(false);
                setFailure(asErrorEnvelope(error)?.message ?? "Could not remove the project.");
              });
            }}
          >
            {busy ? "Removing…" : "Remove"}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * The sidebar, the same in Chat and Agent: search, a new chat or task,
 * schedules, projects, then the mode's own list, and the person and the
 * core's state at the foot. Models, connectors and skills live in Settings.
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
  onRemoveProject,
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
  /** Remove a project and its records; its folder is not touched. */
  readonly onRemoveProject: (project: ProjectRecord) => Promise<void>;
  readonly openingProject: boolean;
  readonly canOpenProject: boolean;
  readonly personName: string | undefined;
  readonly status: string;
  readonly statusText: string;
}): React.JSX.Element {
  // Projects the person opened or closed against the default, which is
  // open for the project on screen and closed for the rest.
  const [toggled, setToggled] = useState<ReadonlySet<string>>(new Set());
  // Conversations of the other expanded projects, read when expanded.
  const [listed, setListed] = useState<ReadonlyMap<string, readonly ConversationRecord[]>>(
    new Map(),
  );
  const [menu, setMenu] = useState<MenuAt | undefined>();
  const [removing, setRemoving] = useState<ProjectRecord | undefined>();
  const closeMenu = useCallback(() => {
    setMenu(undefined);
  }, []);
  const cancelRemoving = useCallback(() => {
    setRemoving(undefined);
  }, []);

  const isExpanded = (projectId: string): boolean =>
    (projectId === openProjectId) !== toggled.has(projectId);

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

  const projectList =
    projects.length === 0 ? (
      <p className="rail-empty">No projects yet. Open a folder with +.</p>
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
                  title="Right-click for more"
                  onClick={() => {
                    toggleProject(project.projectId);
                  }}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    // From the keyboard there is no pointer: open by the row.
                    const box = event.currentTarget.getBoundingClientRect();
                    const fromKeys = event.clientX === 0 && event.clientY === 0;
                    setMenu({
                      project,
                      x: fromKeys ? box.left + 24 : event.clientX,
                      y: fromKeys ? box.bottom : event.clientY,
                    });
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
                      const selected = selectedConversationId === conversation.conversationId;
                      return (
                        <li key={conversation.conversationId}>
                          <button
                            type="button"
                            className="rail-row rail-row--conversation"
                            data-selected={selected}
                            aria-current={selected ? "true" : undefined}
                            onClick={() => {
                              onOpenConversation(project.projectId, conversation.conversationId);
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
    );

  return (
    <aside className="rail" aria-label="Projects and conversations">
      <div className="window-drag window-drag--rail" data-tauri-drag-region aria-hidden="true" />
      <div className="brand-lockup" data-tauri-drag-region>
        <Glyph icon="wind" hue="blue" size={30} />
        <strong data-tauri-drag-region>Tramontana</strong>
        <button
          type="button"
          className="icon-button sidebar-toggle"
          aria-label="Hide sidebar"
          onClick={onHide}
        >
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
      <div className="rail-projects-row">
        <NavRow
          icon="folder"
          label="Projects"
          pressed={view === "projects"}
          onClick={() => {
            onView("projects");
          }}
        />
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
          <section aria-label="Projects and their conversations">{projectList}</section>
        )}
      </nav>

      {/* With a name, the person's initial and name; without one, a plain
          Settings row like the others. */}
      <button
        type="button"
        className="rail-row rail-row--nav rail-person"
        aria-pressed={view === "settings"}
        onClick={() => {
          onView("settings");
        }}
      >
        {personName === undefined || personName.trim() === "" ? (
          <>
            <Icon name="gear" />
            <span className="rail-person-name">Settings</span>
          </>
        ) : (
          <>
            <span className="rail-avatar" aria-hidden="true">
              {(personName.trim()[0] ?? "").toUpperCase()}
            </span>
            <span className="rail-person-name">
              {personName}
              <span className="rail-person-sub"> · Settings</span>
            </span>
            <Icon name="gear" size={16} />
          </>
        )}
      </button>
      <footer className="rail-status">
        <span className="status-dot" data-state={status} aria-hidden="true" />
        <span>{statusText}</span>
      </footer>

      {menu !== undefined && (
        <ProjectMenu
          at={menu}
          onClose={closeMenu}
          onNewTask={() => {
            setMenu(undefined);
            onNewTaskIn(menu.project.projectId);
          }}
          onRemove={() => {
            setMenu(undefined);
            setRemoving(menu.project);
          }}
        />
      )}
      {removing !== undefined && (
        <RemoveProjectDialog
          project={removing}
          onCancel={cancelRemoving}
          onConfirm={async () => {
            await onRemoveProject(removing);
            setRemoving(undefined);
          }}
        />
      )}
    </aside>
  );
}
