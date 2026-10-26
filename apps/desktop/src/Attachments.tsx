import { useCallback, useState } from "react";
import type { TaskAttachment } from "@tramontana/shared";

import { errorMessage, chooseAttachments, discardAttachment } from "./core.js";
import type { StagedAttachment } from "./core.js";
import { Glyph } from "./Glyph.js";

/** The glyph colour for a file, by what it is: the design's red for PDF. */
function hueOf(name: string): "red" | "blue" | "teal" {
  const extension = name.toLowerCase().split(".").pop() ?? "";
  if (extension === "pdf") return "red";
  if (extension === "docx") return "blue";
  return "teal";
}

/** A size the way a person reads one. */
export function describeSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/**
 * The files picked for the next message, and the button that picks them
 * (ADR 0058). The desktop reads and holds each file; this only ever sees
 * its name, its size and the token that attaches it.
 */
export function useAttachments(): {
  readonly files: readonly StagedAttachment[];
  readonly error: string | undefined;
  readonly choose: () => void;
  readonly remove: (token: string) => void;
  /** The tokens of the files picked so far, to send with a message. */
  readonly tokens: () => readonly string[];
  /**
   * Forget the files a message carried, once it has started. Not before: a
   * message that fails to start keeps its files for the next try.
   */
  readonly sent: (tokens: readonly string[]) => void;
} {
  const [files, setFiles] = useState<readonly StagedAttachment[]>([]);
  const [error, setError] = useState<string | undefined>();

  const choose = useCallback(() => {
    setError(undefined);
    void chooseAttachments().then(
      (picked) => {
        setFiles((current) => [...current, ...picked]);
      },
      (failure: unknown) => {
        setError(errorMessage(failure, "Could not attach the files."));
      },
    );
  }, []);

  const remove = useCallback((token: string) => {
    setFiles((current) => current.filter((file) => file.token !== token));
    void discardAttachment(token).catch(() => undefined);
  }, []);

  const tokens = useCallback(() => files.map((file) => file.token), [files]);

  // Only those sent: a file picked while the message was starting stays.
  const sent = useCallback((tokens: readonly string[]) => {
    setFiles((current) => current.filter((file) => !tokens.includes(file.token)));
    setError(undefined);
  }, []);

  return { files, error, choose, remove, tokens, sent };
}

/** The composer's attach button. */
export function AttachButton({
  onClick,
  disabled,
}: {
  readonly onClick: () => void;
  readonly disabled?: boolean;
}): React.JSX.Element {
  return (
    <button
      type="button"
      className="attach-button"
      aria-label="Attach files"
      title="Attach text, PDF or Word files"
      onClick={onClick}
      disabled={disabled}
    >
      <svg
        width="18"
        height="18"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        aria-hidden="true"
      >
        <path d="M12 5v14M5 12h14" />
      </svg>
    </button>
  );
}

/** The files waiting to be sent, each removable. */
export function PendingFiles({
  files,
  onRemove,
}: {
  readonly files: readonly StagedAttachment[];
  readonly onRemove: (token: string) => void;
}): React.JSX.Element | null {
  if (files.length === 0) return null;
  return (
    <ul className="file-chips" aria-label="Attached files">
      {files.map((file) => (
        <li key={file.token} className="file-chip">
          <Glyph icon="doc" hue={hueOf(file.name)} size={26} />
          <span>
            <strong>{file.name}</strong>
            <small>{describeSize(file.byteCount)}</small>
          </span>
          <button
            type="button"
            className="file-chip-remove"
            aria-label={`Remove ${file.name}`}
            onClick={() => {
              onRemove(file.token);
            }}
          >
            ×
          </button>
        </li>
      ))}
    </ul>
  );
}

/** The files a sent message carried, as the record holds them. */
export function SentFiles({
  files,
}: {
  readonly files: readonly TaskAttachment[];
}): React.JSX.Element | null {
  if (files.length === 0) return null;
  return (
    <ul className="file-chips file-chips--sent" aria-label="Files sent with this message">
      {files.map((file, index) => (
        <li key={`${file.contentHash}:${index}`} className="file-chip">
          <Glyph icon="doc" hue={hueOf(file.name)} size={34} />
          <span>
            <strong>{file.name}</strong>
            <small>{describeSize(file.byteCount)}</small>
          </span>
        </li>
      ))}
    </ul>
  );
}
