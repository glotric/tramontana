import { useEffect, useRef, useState } from "react";
import type { ConversationRecord } from "@tramontana/shared";

import { errorMessage, deleteConversation, renameConversation } from "./core.js";
import { Failure } from "./Failure.js";

/**
 * Rename or delete the open conversation (ADR 0059). Deleting asks first, in
 * words that say what goes with it, because it cannot be undone.
 */
export function ConversationMenu({
  conversation,
  onRenamed,
  onDeleted,
}: {
  readonly conversation: ConversationRecord;
  readonly onRenamed: (renamed: ConversationRecord) => void;
  readonly onDeleted: (conversationId: string) => void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<"menu" | "rename" | "delete">("menu");
  const [title, setTitle] = useState(conversation.title);
  const [failure, setFailure] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const titleInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (mode === "rename") titleInput.current?.select();
  }, [mode]);

  const close = (): void => {
    setOpen(false);
    setMode("menu");
    setFailure(undefined);
    setTitle(conversation.title);
  };

  const fail = (error: unknown, fallback: string): void => {
    setFailure(errorMessage(error, fallback));
  };

  return (
    <div className="conversation-menu">
      <button
        type="button"
        className="icon-button"
        aria-label="Conversation options"
        aria-expanded={open}
        onClick={() => {
          if (open) close();
          else setOpen(true);
        }}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <circle cx="5" cy="12" r="1.6" />
          <circle cx="12" cy="12" r="1.6" />
          <circle cx="19" cy="12" r="1.6" />
        </svg>
      </button>
      {open && (
        <div
          className="menu-popover"
          role={mode === "delete" ? "alertdialog" : "dialog"}
          aria-label={mode === "delete" ? "Delete this conversation?" : "Conversation options"}
          onKeyDown={(pressed) => {
            if (pressed.key === "Escape") close();
          }}
        >
          {mode === "menu" && (
            <>
              <button
                type="button"
                className="menu-item"
                onClick={() => {
                  setMode("rename");
                }}
              >
                Rename
              </button>
              <button
                type="button"
                className="menu-item menu-item--danger"
                onClick={() => {
                  setMode("delete");
                }}
              >
                Delete
              </button>
            </>
          )}
          {mode === "rename" && (
            <form
              className="menu-form"
              onSubmit={(submitted) => {
                submitted.preventDefault();
                setBusy(true);
                void renameConversation(conversation.conversationId, title)
                  .then(
                    (renamed) => {
                      onRenamed(renamed);
                      close();
                    },
                    (error: unknown) => {
                      fail(error, "Could not rename the conversation.");
                    },
                  )
                  .finally(() => {
                    setBusy(false);
                  });
              }}
            >
              <label htmlFor="conversation-rename">Title</label>
              <input
                ref={titleInput}
                id="conversation-rename"
                value={title}
                onChange={(changed) => {
                  setTitle(changed.target.value);
                }}
              />
              <div className="menu-actions">
                <button type="button" className="button button--secondary" onClick={close}>
                  Cancel
                </button>
                <button
                  type="submit"
                  className="button button--primary"
                  disabled={busy || title.trim() === ""}
                >
                  Save
                </button>
              </div>
            </form>
          )}
          {mode === "delete" && (
            <div className="menu-form">
              <p>
                Delete “{conversation.title}”? Every message, task and record in it is removed from
                this computer. This can&apos;t be undone.
              </p>
              <div className="menu-actions">
                <button type="button" className="button button--secondary" onClick={close}>
                  Cancel
                </button>
                <button
                  type="button"
                  className="button button--danger"
                  disabled={busy}
                  onClick={() => {
                    setBusy(true);
                    void deleteConversation(conversation.conversationId)
                      .then(
                        () => {
                          close();
                          onDeleted(conversation.conversationId);
                        },
                        (error: unknown) => {
                          fail(error, "Could not delete the conversation.");
                        },
                      )
                      .finally(() => {
                        setBusy(false);
                      });
                  }}
                >
                  Delete
                </button>
              </div>
            </div>
          )}
          <Failure text={failure} />
        </div>
      )}
    </div>
  );
}
