import { useCallback, useEffect, useId, useMemo, useState } from "react";
import type { PersonalSkill } from "@tramontana/shared";

import { listPersonalSkills } from "./core.js";
import { Icon } from "./icons.js";

/** A skill `/` can offer: one of the person's own, or the open project's. */
export interface SkillOption {
  readonly kind: "personal" | "project";
  readonly id: string;
  readonly name: string;
  readonly description?: string;
  /** Suggested for the goal as typed (ADR 0054); project skills only. */
  readonly suggested?: boolean;
}

const keyOf = (option: SkillOption): string => `${option.kind}:${option.id}`;

/** The `/word` being typed at the end of the draft, if any. */
function slashQuery(draft: string): string | undefined {
  const match = /(?:^|\s)\/([\w-]*)$/.exec(draft);
  return match?.[1];
}

/** The person's own skills, read when asked for. */
export function usePersonalSkills(): {
  readonly skills: readonly PersonalSkill[];
  readonly refresh: () => void;
} {
  const [skills, setSkills] = useState<readonly PersonalSkill[]>([]);
  const refresh = useCallback(() => {
    void listPersonalSkills().then(setSkills, () => undefined);
  }, []);
  useEffect(refresh, [refresh]);
  return { skills, refresh };
}

/**
 * Choosing skills with `/` in a composer (ADR 0070), as Claude does: type
 * `/` and part of a name, pick one, and it rides along with the next
 * message as a chip. A skill is guidance and grants nothing; choosing one
 * is said in the line under the composer.
 */
export function useSkillSlash({
  draft,
  setDraft,
  personal,
  project = [],
  onOpen,
}: {
  readonly draft: string;
  readonly setDraft: (draft: string) => void;
  readonly personal: readonly PersonalSkill[];
  readonly project?: readonly SkillOption[];
  /** Asked when the menu opens, to read the library again. */
  readonly onOpen?: () => void;
}) {
  const [chosen, setChosen] = useState<readonly SkillOption[]>([]);
  const [active, setActive] = useState(0);
  // Escape puts the menu away until the `/word` changes.
  const [dismissed, setDismissed] = useState<string | undefined>();

  const query = slashQuery(draft);
  const open = query !== undefined && query !== dismissed;

  const [wasOpen, setWasOpen] = useState(false);
  if (open !== wasOpen) {
    setWasOpen(open);
    setActive(0);
  }
  // Read the library again each time the menu opens: a skill imported in
  // Settings a moment ago is offered at once.
  useEffect(() => {
    if (open) onOpen?.();
  }, [open, onOpen]);

  const options = useMemo<readonly SkillOption[]>(() => {
    const own: SkillOption[] = personal.map((skill) => ({
      kind: "personal",
      id: skill.id,
      name: skill.name,
      ...(skill.description === undefined ? {} : { description: skill.description }),
    }));
    // Suggested project skills first, then the person's, then the rest.
    return [
      ...project.filter((skill) => skill.suggested === true),
      ...own,
      ...project.filter((skill) => skill.suggested !== true),
    ];
  }, [personal, project]);

  const matches = useMemo(() => {
    if (!open) return [];
    const needle = query.toLowerCase();
    return options.filter(
      (option) =>
        !chosen.some((picked) => keyOf(picked) === keyOf(option)) &&
        (option.id.toLowerCase().includes(needle) || option.name.toLowerCase().includes(needle)),
    );
  }, [chosen, open, options, query]);

  const pick = useCallback(
    (option: SkillOption) => {
      setChosen((current) =>
        current.some((picked) => keyOf(picked) === keyOf(option)) ? current : [...current, option],
      );
      // The `/word` was only how the skill was asked for.
      setDraft(draft.replace(/(^|\s)\/[\w-]*$/, "$1"));
    },
    [draft, setDraft],
  );

  const remove = useCallback((option: SkillOption) => {
    setChosen((current) => current.filter((picked) => keyOf(picked) !== keyOf(option)));
  }, []);

  /** Keys the menu takes before the composer sees them; true if taken. */
  const onKeyDown = useCallback(
    (event: React.KeyboardEvent): boolean => {
      if (!open) return false;
      if (event.key === "Escape") {
        setDismissed(query);
        return true;
      }
      if (matches.length === 0) return false;
      if (event.key === "ArrowDown") {
        setActive((current) => (current + 1) % matches.length);
        return true;
      }
      if (event.key === "ArrowUp") {
        setActive((current) => (current - 1 + matches.length) % matches.length);
        return true;
      }
      if ((event.key === "Enter" && !event.shiftKey) || event.key === "Tab") {
        const option = matches[Math.min(active, matches.length - 1)];
        if (option !== undefined) pick(option);
        return true;
      }
      return false;
    },
    [active, matches, open, pick, query],
  );

  return {
    open,
    matches,
    active: Math.min(active, Math.max(0, matches.length - 1)),
    chosen,
    pick,
    remove,
    onKeyDown,
    /** Forget the chosen skills: they went with the message just sent. */
    clear: useCallback(() => {
      setChosen([]);
    }, []),
    personalIds: chosen.filter((option) => option.kind === "personal").map((option) => option.id),
    projectIds: chosen.filter((option) => option.kind === "project").map((option) => option.id),
  };
}

export type SkillSlash = ReturnType<typeof useSkillSlash>;

/** The `/` menu, above the composer while a `/word` is being typed. */
export function SlashMenu({
  slash,
  hasAny,
}: {
  readonly slash: SkillSlash;
  /** Whether there are any skills at all, to say how to add some if not. */
  readonly hasAny: boolean;
}): React.JSX.Element | null {
  const list = useId();
  if (!slash.open) return null;
  return (
    <div className="slash-menu">
      {slash.matches.length === 0 ? (
        <p className="slash-empty">
          {hasAny
            ? "No skill matches. Keep typing, or press Escape."
            : "No skills yet. Add your own in Settings → Customize, or in a project's .tramontana/skills folder."}
        </p>
      ) : (
        <ul id={list} role="listbox" aria-label="Skills">
          {slash.matches.map((option, index) => (
            <li
              key={keyOf(option)}
              role="option"
              aria-selected={index === slash.active}
              data-active={index === slash.active}
              onMouseDown={(event) => {
                // Picked before the box loses focus.
                event.preventDefault();
                slash.pick(option);
              }}
            >
              <Icon name="sparkle" size={15} />
              <span className="slash-name">
                /{option.id}
                {option.name !== option.id && <span className="slash-title"> {option.name}</span>}
              </span>
              <span className="slash-meta">
                {option.suggested === true
                  ? "suggested for this goal"
                  : option.kind === "project"
                    ? "this project"
                    : "your skill"}
              </span>
              {option.description !== undefined && (
                <span className="slash-description">{option.description}</span>
              )}
            </li>
          ))}
        </ul>
      )}
      <p className="slash-hint">↑↓ to choose · Enter to add · Esc to close</p>
    </div>
  );
}

/** The skills chosen for the next message, each removable. */
export function ChosenSkills({ slash }: { readonly slash: SkillSlash }): React.JSX.Element | null {
  if (slash.chosen.length === 0) return null;
  return (
    <ul className="file-chips skill-chips" aria-label="Skills for this message">
      {slash.chosen.map((option) => (
        <li key={keyOf(option)} className="file-chip skill-chip">
          <Icon name="sparkle" size={15} />
          <span>
            <strong>/{option.id}</strong>
            <small>{option.kind === "project" ? "this project" : "your skill"}</small>
          </span>
          <button
            type="button"
            className="file-chip-remove"
            aria-label={`Remove the skill ${option.name}`}
            onClick={() => {
              slash.remove(option);
            }}
          >
            ×
          </button>
        </li>
      ))}
    </ul>
  );
}
