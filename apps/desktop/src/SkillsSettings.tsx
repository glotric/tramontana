import { useCallback, useEffect, useState } from "react";
import type { PersonalSkill } from "@tramontana/shared";

import {
  asErrorEnvelope,
  importSkill,
  listPersonalSkills,
  removeSkill,
  skillsFolder,
} from "./core.js";
import { Icon } from "./icons.js";

function message(error: unknown, fallback: string): string {
  return asErrorEnvelope(error)?.message ?? fallback;
}

function describeSize(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : `${Math.round(bytes / 1024)} KB`;
}

/**
 * Settings, Customize, Skills (ADR 0070): the person's own skills, kept as
 * folders beside the profile. Each is imported from a skill folder, a `.zip`
 * or a `.md` file; only its `SKILL.md` is kept, and nothing in it runs.
 */
export function SkillsSettings(): React.JSX.Element {
  const [skills, setSkills] = useState<readonly PersonalSkill[] | undefined>();
  const [folder, setFolder] = useState<string | undefined>();
  const [failure, setFailure] = useState<string | undefined>();
  const [note, setNote] = useState<string | undefined>();
  const [removing, setRemoving] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);

  const read = useCallback(() => {
    void listPersonalSkills().then(setSkills, (error: unknown) => {
      setSkills([]);
      setFailure(message(error, "Could not read your skills."));
    });
  }, []);

  useEffect(() => {
    read();
    void skillsFolder().then(setFolder, () => undefined);
  }, [read]);

  const add = (fromFolder: boolean): void => {
    setBusy(true);
    setFailure(undefined);
    setNote(undefined);
    void importSkill(fromFolder)
      .then(
        (added) => {
          if (added === undefined) return;
          setNote(
            (added.redactions ?? []).length > 0
              ? `Added /${added.id}. Something that looked like a credential was taken out of it.`
              : `Added /${added.id}. Type /${added.id} in a composer to use it.`,
          );
          read();
        },
        (error: unknown) => {
          setFailure(message(error, "Could not import the skill."));
        },
      )
      .finally(() => {
        setBusy(false);
      });
  };

  return (
    <div className="settings-card skills-settings">
      <h2 id="skills-heading" className="settings-part-heading">
        Skills
      </h2>
      <p className="settings-hint">
        A skill is a set of instructions a task follows when you choose it: type <kbd>/</kbd> in
        Chat or Agent. Choosing a skill gives the task its text and nothing else; it never lets a
        task do more than you allow.
      </p>

      <div className="settings-actions">
        <button
          type="button"
          className="button button--primary"
          disabled={busy}
          onClick={() => {
            add(true);
          }}
        >
          Import a skill folder…
        </button>
        <button
          type="button"
          className="button button--secondary"
          disabled={busy}
          onClick={() => {
            add(false);
          }}
        >
          Import a .zip or .md file…
        </button>
      </div>
      <p className="settings-hint skills-where">
        Only a skill&apos;s SKILL.md is kept; its scripts and other files are not imported, and
        nothing in a skill is run. Yours are kept on this computer
        {folder === undefined ? "." : ":"}
        {folder !== undefined && <code>{folder}</code>}
      </p>

      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}
      {note !== undefined && (
        <p className="notice" role="status">
          {note}
        </p>
      )}

      {skills === undefined ? (
        <p className="inline-empty">Reading your skills…</p>
      ) : skills.length === 0 ? (
        <p className="inline-empty">You have no skills of your own yet.</p>
      ) : (
        <ul className="skill-list" aria-labelledby="skills-heading">
          {skills.map((skill) => (
            <li key={skill.id}>
              <Icon name="sparkle" size={16} />
              <div className="skill-list-text">
                <strong>
                  {skill.name} <code>/{skill.id}</code>
                </strong>
                {skill.description !== undefined && <span>{skill.description}</span>}
                <small>{describeSize(skill.byteCount)}</small>
              </div>
              {removing === skill.id ? (
                <div
                  className="skill-list-confirm"
                  role="group"
                  aria-label={`Remove /${skill.id}?`}
                >
                  <span>Remove it? Tasks that used it keep what they read.</span>
                  <button
                    type="button"
                    className="button button--secondary"
                    onClick={() => {
                      setRemoving(undefined);
                    }}
                  >
                    Keep
                  </button>
                  <button
                    type="button"
                    className="button button--danger"
                    onClick={() => {
                      setFailure(undefined);
                      void removeSkill(skill.id).then(
                        () => {
                          setRemoving(undefined);
                          setNote(`Removed /${skill.id}.`);
                          read();
                        },
                        (error: unknown) => {
                          setFailure(message(error, "Could not remove the skill."));
                        },
                      );
                    }}
                  >
                    Remove
                  </button>
                </div>
              ) : (
                <button
                  type="button"
                  className="button button--secondary"
                  aria-label={`Remove the skill ${skill.name}`}
                  onClick={() => {
                    setRemoving(skill.id);
                  }}
                >
                  Remove…
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      <p className="settings-hint">
        Projects can offer skills too, in their <code>.tramontana/skills/</code> folder. Those are
        part of the project, are edited there, and are offered only in that project.
      </p>
    </div>
  );
}
