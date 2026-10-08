import type { ProjectRecord, RoutingMode } from "@tramontana/shared";

import { AttachButton, PendingFiles } from "./Attachments.js";
import type { useAttachments } from "./Attachments.js";
import { MODE_LABEL } from "./cards.js";
import { ChipMenu } from "./ChipMenu.js";
import { MODES, MODE_TEXT, describeAccess, describeChoices } from "./choices.js";
import { DictateButton } from "./Dictation.js";
import type { useDictation } from "./Dictation.js";
import { Icon } from "./icons.js";
import { ChosenSkills, SlashMenu } from "./SkillSlash.js";
import type { SkillSlash } from "./SkillSlash.js";
import { ADMISSIONS } from "./useComposerOptions.js";
import type { useComposerOptions } from "./useComposerOptions.js";

/** What a routing mode means, without repeating its name. */
function explain(mode: RoutingMode): string {
  const text = MODE_TEXT[mode].replace(/^[^:]+: /, "");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * The Agent composer: one box for the task, and above it a row of chips
 * that each say a choice the task starts with — the project, where models
 * run, what it may do, its skills and its worker. Every choice is still
 * said in words, and the line under the box says what they add up to
 * before anything starts.
 */
export function AgentComposer({
  goal,
  onGoal,
  goalInput,
  options,
  projects,
  project,
  onChooseProject,
  onOpenProject,
  files,
  dictation,
  running,
  canStart,
  onStart,
  onCancel,
  slash,
  hasSkills,
}: {
  readonly goal: string;
  readonly onGoal: (goal: string) => void;
  readonly goalInput: React.RefObject<HTMLTextAreaElement | null>;
  readonly options: ReturnType<typeof useComposerOptions>;
  readonly projects: readonly ProjectRecord[];
  readonly project: ProjectRecord | undefined;
  readonly onChooseProject: (projectId: string) => void;
  readonly onOpenProject: () => void;
  readonly files: ReturnType<typeof useAttachments>;
  readonly dictation: ReturnType<typeof useDictation>;
  readonly running: boolean;
  readonly canStart: boolean;
  readonly onStart: () => void;
  readonly onCancel: () => void;
  /** Skills chosen with `/` for this message (ADR 0070). */
  readonly slash: SkillSlash;
  /** Whether any skill exists to offer, for the menu's empty line. */
  readonly hasSkills: boolean;
}): React.JSX.Element {
  const { routingMode, admission, chosenWorker } = options;

  return (
    <div className="composer-dock">
      <div className="composer-chips" role="group" aria-label="How this task may run">
        <ChipMenu
          name="Project"
          value={project?.name ?? "Choose a project"}
          icon="folder"
          disabled={running}
        >
          {(close) => (
            <div className="chip-list">
              {projects.map((choice) => (
                <button
                  key={choice.projectId}
                  type="button"
                  aria-pressed={choice.projectId === project?.projectId}
                  onClick={() => {
                    close();
                    onChooseProject(choice.projectId);
                  }}
                >
                  <Icon name="folder" size={15} />
                  <span>{choice.name}</span>
                </button>
              ))}
              <button
                type="button"
                onClick={() => {
                  close();
                  onOpenProject();
                }}
              >
                <Icon name="plus" size={15} />
                <span>Open another folder…</span>
              </button>
            </div>
          )}
        </ChipMenu>

        <ChipMenu
          name="Where models run"
          value={MODE_LABEL[routingMode]}
          icon={routingMode === "localOnly" ? "lock" : "cpu"}
          disabled={running}
        >
          {(close) => (
            <fieldset className="chip-choices">
              <legend className="sr-only">Where models may run</legend>
              {MODES.map((mode) => (
                <div key={mode} className="chip-choice" data-selected={routingMode === mode}>
                  <label>
                    <input
                      type="radio"
                      name="routing-mode"
                      value={mode}
                      checked={routingMode === mode}
                      aria-describedby={`routing-${mode}`}
                      onChange={() => {
                        options.setRoutingMode(mode);
                        close();
                      }}
                    />
                    {MODE_LABEL[mode]}
                  </label>
                  <p id={`routing-${mode}`}>{explain(mode)}</p>
                </div>
              ))}
            </fieldset>
          )}
        </ChipMenu>

        <ChipMenu name="Access" value={describeAccess(admission)} icon="shield" disabled={running}>
          {() => (
            <fieldset className="chip-choices">
              <legend className="chip-legend">
                Read-only unless you allow more. Each action is still asked about before it runs.
              </legend>
              {ADMISSIONS.map(({ key, label }) => (
                <label key={key} className="chip-toggle">
                  <input
                    type="checkbox"
                    checked={admission[key]}
                    onChange={(changed) => {
                      options.allow(key, changed.target.checked);
                    }}
                  />
                  {label}
                </label>
              ))}
            </fieldset>
          )}
        </ChipMenu>

        {options.offeredWorkers.length > 1 && (
          <ChipMenu
            name="Worker"
            value={chosenWorker?.displayName ?? "Worker"}
            icon="grid"
            disabled={running}
          >
            {() => (
              <label className="chip-select">
                Worker
                <select
                  value={chosenWorker?.agentId ?? ""}
                  onChange={(changed) => {
                    options.setWorker(changed.target.value);
                  }}
                >
                  {options.offeredWorkers.map((choice) => (
                    <option key={choice.agentId} value={choice.agentId}>
                      {choice.displayName}
                      {choice.cloud === true ? " (off this device)" : ""}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </ChipMenu>
        )}
      </div>

      <SlashMenu slash={slash} hasAny={hasSkills} />
      <form
        className="composer"
        onSubmit={(submitted) => {
          submitted.preventDefault();
          if (canStart) onStart();
        }}
      >
        <label className="sr-only" htmlFor="goal">
          Task goal
        </label>
        <PendingFiles files={files.files} onRemove={files.remove} />
        <ChosenSkills slash={slash} />
        <textarea
          ref={goalInput}
          id="goal"
          value={goal}
          placeholder={
            project === undefined
              ? "Open a project to start a task"
              : `Describe a task, or ask about ${project.name}. Type / for skills.`
          }
          onChange={(changed) => {
            onGoal(changed.target.value);
          }}
          onKeyDown={(pressed) => {
            if (slash.onKeyDown(pressed)) {
              pressed.preventDefault();
              return;
            }
            if (pressed.key === "Enter" && !pressed.shiftKey && !pressed.nativeEvent.isComposing) {
              pressed.preventDefault();
              if (canStart) onStart();
            }
          }}
          disabled={running}
          rows={2}
        />
        <div className="composer-footer">
          <AttachButton onClick={files.choose} disabled={running} />
          <span />
          <DictateButton
            state={dictation.state}
            seconds={dictation.seconds}
            onToggle={dictation.toggle}
          />
          {running ? (
            <button type="button" className="button button--stop" onClick={onCancel}>
              <span className="stop-square" aria-hidden="true" />
              Stop
            </button>
          ) : (
            <button
              type="submit"
              className="send-button"
              aria-label="Start task"
              disabled={!canStart}
            >
              <Icon name="send" size={18} strokeWidth={2.2} />
            </button>
          )}
        </div>
      </form>
      <p className="composer-disclosure">
        {describeChoices(routingMode, admission, slash.chosen.length)}
      </p>
    </div>
  );
}
