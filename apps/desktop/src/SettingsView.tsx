import { useEffect, useState } from "react";
import type { AnswerStyle, PersonalProfile } from "@tramontana/shared";

import { errorMessage, forget, personalProfile, remember, updatePersonal } from "./core.js";
import { DictationSettings } from "./Dictation.js";
import { EraseAllData } from "./EraseAllData.js";
import { Glyph } from "./Glyph.js";
import { Segmented, segmentedTabId } from "./Segmented.js";
import { chooseTheme, rememberedTheme } from "./theme.js";
import type { Theme } from "./theme.js";
import type { GlyphHue, GlyphIcon } from "./Glyph.js";

const STYLES: readonly {
  readonly id: AnswerStyle;
  readonly label: string;
  readonly description: string;
  readonly icon: GlyphIcon;
  readonly hue: GlyphHue;
}[] = [
  {
    id: "quick",
    label: "Quick",
    description: "Short answers, as fast as possible.",
    icon: "clock",
    hue: "yellow",
  },
  {
    id: "balanced",
    label: "Balanced",
    description: "Clear, complete answers for everyday work.",
    icon: "check",
    hue: "blue",
  },
  {
    id: "thorough",
    label: "Thorough",
    description: "Takes longer and thinks things through.",
    icon: "search",
    hue: "purple",
  },
];

/**
 * The design's Settings, Personalization (ADR 0061): what chats are told
 * about the person, and the notes they remember. Everything here is the
 * person's own words, shown in full; nothing is remembered on its own.
 */
const THEMES: readonly (readonly [Theme, string])[] = [
  ["system", "Same as this computer"],
  ["light", "Light"],
  ["dark", "Dark"],
];

/** Settings, General, Appearance: light, dark, or as the computer is. */
function AppearanceSettings(): React.JSX.Element {
  const [theme, setTheme] = useState<Theme>(rememberedTheme);
  return (
    <fieldset className="settings-section appearance">
      <legend>Appearance</legend>
      <Segmented
        kind="radios"
        className="overview-segments"
        label="Appearance"
        value={theme}
        options={THEMES.map(([value, label]) => ({ value, label }))}
        onChange={(value) => {
          chooseTheme(value);
          setTheme(value);
        }}
      />
    </fieldset>
  );
}

function GeneralSettings(): React.JSX.Element {
  const [profile, setProfile] = useState<PersonalProfile | undefined>();
  const [name, setName] = useState("");
  const [about, setAbout] = useState("");
  const [style, setStyle] = useState<AnswerStyle>("balanced");
  const [memory, setMemory] = useState(true);
  const [note, setNote] = useState("");
  const [failure, setFailure] = useState<string | undefined>();
  const [saved, setSaved] = useState(false);

  const show = (loaded: PersonalProfile): void => {
    setProfile(loaded);
    setName(loaded.name ?? "");
    setAbout(loaded.about ?? "");
    setStyle(loaded.answerStyle);
    setMemory(loaded.memoryEnabled);
  };

  useEffect(() => {
    let active = true;
    void personalProfile().then(
      (loaded) => {
        if (active) show(loaded);
      },
      (error: unknown) => {
        if (active) setFailure(errorMessage(error, "Could not read your profile."));
      },
    );
    return () => {
      active = false;
    };
  }, []);

  const changed =
    profile !== undefined &&
    (name.trim() !== (profile.name ?? "") ||
      about.trim() !== (profile.about ?? "") ||
      style !== profile.answerStyle ||
      memory !== profile.memoryEnabled);

  return (
    <div className="settings-card">
      <div className="settings-intro">
        <Glyph icon="lock" hue="green" size={48} />
        <p>
          What you write here is kept on this computer and given only to chats, never to Agent
          tasks. It is marked as sensitive: you are asked before it goes to a cloud model, and Local
          only keeps it on this device.
        </p>
      </div>

      {failure !== undefined && (
        <p className="notice notice--error" role="alert">
          {failure}
        </p>
      )}

      <form
        className="settings-section"
        onSubmit={(submitted) => {
          submitted.preventDefault();
          setFailure(undefined);
          void updatePersonal({ name, about, answerStyle: style, memoryEnabled: memory }).then(
            (updated) => {
              show(updated);
              setSaved(true);
            },
            (error: unknown) => {
              setFailure(errorMessage(error, "Could not save your profile."));
            },
          );
        }}
      >
        <label htmlFor="personal-name">What should Tramontana call you?</label>
        <input
          id="personal-name"
          value={name}
          maxLength={100}
          onChange={(event) => {
            setName(event.target.value);
            setSaved(false);
          }}
        />
        <label htmlFor="personal-about">What do you do at work?</label>
        <p className="settings-hint">A sentence or two helps chats give more useful answers.</p>
        <textarea
          id="personal-about"
          rows={3}
          value={about}
          maxLength={1000}
          placeholder="For example: I plan maintenance for our crane systems."
          onChange={(event) => {
            setAbout(event.target.value);
            setSaved(false);
          }}
        />

        <fieldset className="style-choices">
          <legend>How should Tramontana answer?</legend>
          {STYLES.map((choice) => (
            <label key={choice.id} className="style-choice" data-selected={style === choice.id}>
              <input
                type="radio"
                name="answer-style"
                value={choice.id}
                checked={style === choice.id}
                onChange={() => {
                  setStyle(choice.id);
                  setSaved(false);
                }}
              />
              <Glyph icon={choice.icon} hue={choice.hue} size={40} />
              <strong>{choice.label}</strong>
              <span>{choice.description}</span>
            </label>
          ))}
        </fieldset>

        <label className="switch-row">
          <span>
            <strong>Remember things between chats</strong>
            <span>
              Chats are given the notes below. Notes are kept when this is off, but not sent.
            </span>
          </span>
          <input
            type="checkbox"
            role="switch"
            className="switch"
            checked={memory}
            onChange={(event) => {
              setMemory(event.target.checked);
              setSaved(false);
            }}
          />
        </label>

        <div className="menu-actions">
          {saved && !changed && (
            <span className="settings-saved" role="status">
              Saved
            </span>
          )}
          <button
            type="submit"
            className="button button--primary"
            disabled={profile === undefined || !changed}
          >
            Save
          </button>
        </div>
      </form>

      <section className="settings-section" aria-labelledby="notes-heading">
        <h2 id="notes-heading">Notes to remember</h2>
        <p className="settings-hint">
          Only what you add here. Tramontana does not remember anything on its own.
        </p>
        {profile !== undefined && profile.notes.length > 0 ? (
          <ul className="memory-notes" aria-label="Notes">
            {profile.notes.map((kept) => (
              <li key={kept.noteId}>
                <span>{kept.text}</span>
                <button
                  type="button"
                  className="file-chip-remove"
                  aria-label={`Forget “${kept.text}”`}
                  onClick={() => {
                    void forget(kept.noteId).then(show, (error: unknown) => {
                      setFailure(errorMessage(error, "Could not forget the note."));
                    });
                  }}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="inline-empty">No notes yet.</p>
        )}
        <form
          className="new-conversation"
          onSubmit={(submitted) => {
            submitted.preventDefault();
            setFailure(undefined);
            void remember(note).then(
              (updated) => {
                show(updated);
                setNote("");
              },
              (error: unknown) => {
                setFailure(errorMessage(error, "Could not add the note."));
              },
            );
          }}
        >
          <label className="sr-only" htmlFor="memory-note">
            New note
          </label>
          <input
            id="memory-note"
            value={note}
            maxLength={500}
            placeholder="For example: Our site is in Koper."
            onChange={(event) => {
              setNote(event.target.value);
            }}
          />
          <button type="submit" disabled={note.trim() === ""}>
            Remember
          </button>
        </form>
      </section>

      <DictationSettings />
    </div>
  );
}

export type SettingsTab = "general" | "customize" | "models" | "data";

const TABS: readonly (readonly [SettingsTab, string])[] = [
  ["general", "General"],
  ["customize", "Customize"],
  ["models", "Models and credentials"],
  ["data", "Data"],
];

/**
 * Settings, in four parts: what chats are told about the person and how
 * they dictate; what Tramontana is extended with (skills and connectors);
 * the models it runs on; and the profile's data.
 */
export function SettingsView({
  tab,
  onTab,
  customize,
  models,
}: {
  readonly tab: SettingsTab;
  readonly onTab: (tab: SettingsTab) => void;
  /** The Customize part: skills and connectors. */
  readonly customize: React.ReactNode;
  /** The Models and credentials part. */
  readonly models: React.ReactNode;
}): React.JSX.Element {
  return (
    <section className="settings-view" aria-labelledby="settings-heading">
      <h1 id="settings-heading">Settings</h1>
      <Segmented
        kind="tabs"
        className="settings-tabs overview-segments"
        label="Settings sections"
        idPrefix="settings"
        panelId="settings-panel"
        value={tab}
        options={TABS.map(([value, label]) => ({ value, label }))}
        onChange={onTab}
      />
      <div
        className="settings-panel"
        role="tabpanel"
        id="settings-panel"
        aria-labelledby={segmentedTabId("settings", tab)}
      >
        {tab === "general" ? (
          <div className="settings-parts">
            <div className="settings-card">
              <AppearanceSettings />
            </div>
            <GeneralSettings />
          </div>
        ) : tab === "customize" ? (
          customize
        ) : tab === "models" ? (
          models
        ) : (
          <div className="settings-card">
            <EraseAllData />
          </div>
        )}
      </div>
    </section>
  );
}
