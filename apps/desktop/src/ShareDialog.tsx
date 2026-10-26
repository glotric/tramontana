import { useState } from "react";
import type { ConversationRecord, TranscriptFormat } from "@tramontana/shared";

import { errorMessage, previewTranscript, saveTranscript } from "./core.js";
import { Failure } from "./Failure.js";
import { useLoad } from "./useLoad.js";

/**
 * Share a conversation as a file (ADR 0060). The person reads exactly what
 * will be saved, chooses where, and sends the file themselves; nothing is
 * sent anywhere from here.
 */
export function ShareDialog({
  conversation,
  onClose,
}: {
  readonly conversation: ConversationRecord;
  readonly onClose: () => void;
}): React.JSX.Element {
  const [format, setFormat] = useState<TranscriptFormat>("markdown");
  const [includeAttachments, setIncludeAttachments] = useState(false);
  const [text, setText] = useState<string | undefined>();
  const [failure, setFailure] = useState<string | undefined>();
  const [saved, setSaved] = useState<string | undefined>();
  const [saving, setSaving] = useState(false);

  useLoad(
    () => previewTranscript(conversation.conversationId, { format, includeAttachments }),
    [conversation.conversationId, format, includeAttachments],
    (preview) => {
      setText(preview);
      setFailure(undefined);
    },
    (error) => {
      setFailure(errorMessage(error, "Could not prepare the file."));
    },
  );

  return (
    <div
      className="search-backdrop"
      onMouseDown={(pressed) => {
        if (pressed.target === pressed.currentTarget) onClose();
      }}
    >
      <div
        className="share-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="share-heading"
        onKeyDown={(pressed) => {
          if (pressed.key === "Escape") onClose();
        }}
      >
        <h2 id="share-heading">Share “{conversation.title}”</h2>
        <p className="share-note">
          Saves a copy to a file you choose, for you to send however you like. Nothing is sent from
          Tramontana.
        </p>
        <div className="share-options">
          <fieldset className="composer-modes">
            <legend className="sr-only">Format</legend>
            {(["markdown", "html"] as const).map((choice) => (
              <label key={choice} data-selected={format === choice}>
                <input
                  type="radio"
                  name="share-format"
                  value={choice}
                  checked={format === choice}
                  onChange={() => {
                    setFormat(choice);
                  }}
                />
                {choice === "markdown" ? "Markdown" : "Web page"}
              </label>
            ))}
          </fieldset>
          <label className="composer-toggle" data-checked={includeAttachments}>
            <input
              type="checkbox"
              checked={includeAttachments}
              onChange={(changed) => {
                setIncludeAttachments(changed.target.checked);
              }}
            />
            Include attached files&apos; text
          </label>
        </div>
        <pre className="share-preview" role="region" aria-label="What will be saved" tabIndex={0}>
          {text ?? "Preparing…"}
        </pre>
        <Failure text={failure} />
        {saved !== undefined && (
          <p className="notice" role="status">
            Saved to {saved}
          </p>
        )}
        <div className="menu-actions">
          <button type="button" className="button button--secondary" onClick={onClose}>
            {saved === undefined ? "Cancel" : "Done"}
          </button>
          <button
            type="button"
            className="button button--primary"
            disabled={text === undefined || saving}
            onClick={() => {
              setSaving(true);
              void saveTranscript(conversation.conversationId, { format, includeAttachments })
                .then(
                  (path) => {
                    if (path !== undefined) setSaved(path);
                  },
                  (error: unknown) => {
                    setFailure(errorMessage(error, "Could not save the file."));
                  },
                )
                .finally(() => {
                  setSaving(false);
                });
            }}
          >
            Save as file…
          </button>
        </div>
      </div>
    </div>
  );
}
