import { useEffect, useRef, useState } from "react";
import type { ConversationHit, ProjectRecord, SearchResults } from "@tramontana/shared";

import { errorMessage, search } from "./core.js";
import { Glyph } from "./Glyph.js";
import { Failure } from "./Failure.js";

/** How long the typing settles before searching again. */
const SEARCH_DELAY_MS = 200;

/** Why a conversation matched, in words. */
const MATCHED: Record<ConversationHit["matchedIn"], string> = {
  title: "Title",
  goal: "Asked",
  project: "Project",
};

/**
 * Search chats and projects (ADR 0059): titles, what was asked, and project
 * names and folders. Plain text, ignoring case; the rule is core's.
 */
export function SearchPanel({
  onClose,
  onOpenConversation,
  onOpenProject,
}: {
  readonly onClose: () => void;
  readonly onOpenConversation: (hit: ConversationHit) => void;
  readonly onOpenProject: (project: ProjectRecord) => void;
}): React.JSX.Element {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResults | undefined>();
  const [failure, setFailure] = useState<string | undefined>();
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    input.current?.focus();
  }, []);

  useEffect(() => {
    const wanted = query.trim();
    // Nothing to ask for: the panel shows nothing, whatever was found last.
    if (wanted === "") return;
    let active = true;
    const timer = setTimeout(() => {
      void search(wanted).then(
        (found) => {
          if (!active) return;
          setResults(found);
          setFailure(undefined);
        },
        (error: unknown) => {
          if (active) setFailure(errorMessage(error, "Could not search."));
        },
      );
    }, SEARCH_DELAY_MS);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [query]);

  // What was found last is shown only while there is something typed.
  const shown = query.trim() === "" ? undefined : results;
  const nothing = shown?.conversations.length === 0 && shown.projects.length === 0;

  return (
    <div
      className="search-backdrop"
      onMouseDown={(pressed) => {
        if (pressed.target === pressed.currentTarget) onClose();
      }}
    >
      <div
        className="search-panel"
        role="dialog"
        aria-modal="true"
        aria-label="Search"
        onKeyDown={(pressed) => {
          if (pressed.key === "Escape") onClose();
        }}
      >
        <div className="search-field">
          <Glyph icon="search" hue="purple" size={26} />
          <label className="sr-only" htmlFor="search-query">
            Search chats and projects
          </label>
          <input
            ref={input}
            id="search-query"
            type="search"
            value={query}
            placeholder="Search chats and projects"
            autoComplete="off"
            onChange={(changed) => {
              setQuery(changed.target.value);
            }}
          />
        </div>
        <Failure text={query.trim() !== "" ? failure : undefined} />
        {nothing && <p className="search-empty">Nothing matches “{query.trim()}”.</p>}
        {shown !== undefined && shown.conversations.length > 0 && (
          <section aria-labelledby="search-conversations">
            <h2 id="search-conversations" className="rail-heading">
              Conversations
            </h2>
            <ul className="search-results">
              {shown.conversations.map((hit) => (
                <li key={hit.conversation.conversationId}>
                  <button
                    type="button"
                    onClick={() => {
                      onOpenConversation(hit);
                    }}
                  >
                    <strong>{hit.conversation.title}</strong>
                    <span>
                      {hit.projectName ?? "Chat"} · {MATCHED[hit.matchedIn]}
                      {hit.excerpt === undefined ? "" : `: ${hit.excerpt}`}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}
        {shown !== undefined && shown.projects.length > 0 && (
          <section aria-labelledby="search-projects">
            <h2 id="search-projects" className="rail-heading">
              Projects
            </h2>
            <ul className="search-results">
              {shown.projects.map((project) => (
                <li key={project.projectId}>
                  <button
                    type="button"
                    onClick={() => {
                      onOpenProject(project);
                    }}
                  >
                    <strong>{project.name}</strong>
                    <span>{project.roots[0]}</span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </div>
  );
}
