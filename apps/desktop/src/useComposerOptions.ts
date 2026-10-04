import { useEffect, useMemo, useState } from "react";
import type { RoutingMode } from "@tramontana/shared";

import { listSkills, listWorkers, suggestSkills } from "./core.js";
import type { SkillView, WorkerView } from "./core.js";

/** How long typing must settle before skills are suggested again. */
const SUGGESTION_DELAY_MS = 300;

/** What a task may do beyond reading, each off until the person says so. */
export interface Admission {
  readonly writes: boolean;
  readonly recipes: boolean;
  readonly apps: boolean;
  readonly browse: boolean;
}

/** The composer's toggles, in the order it shows them. */
export const ADMISSIONS: readonly { readonly key: keyof Admission; readonly label: string }[] = [
  { key: "writes", label: "May edit files" },
  { key: "recipes", label: "May run recipes" },
  { key: "apps", label: "May use Apps" },
  { key: "browse", label: "May browse the web" },
];

const READ_ONLY: Admission = { writes: false, recipes: false, apps: false, browse: false };

/**
 * The choices a task is started with: where its models may run, what it may
 * do, which worker runs it and which of the project's skills it uses.
 */
export function useComposerOptions(goal: string, projectId: string | undefined) {
  // Read-only and Automatic until the person says otherwise, for each task.
  const [routingMode, setRoutingMode] = useState<RoutingMode>("automatic");
  const [admission, setAdmission] = useState<Admission>(READ_ONLY);
  const [workers, setWorkers] = useState<readonly WorkerView[]>([]);
  const [worker, setWorker] = useState<string>("");
  const [skills, setSkills] = useState<
    { readonly projectId: string; readonly skills: readonly SkillView[] } | undefined
  >();
  const [ticked, setTicked] = useState<readonly string[]>([]);
  const [suggested, setSuggested] = useState<readonly string[]>([]);

  // The workers a task can run on (ADR 0048). Without any, the composer
  // shows no choice and every task runs the built-in worker.
  useEffect(() => {
    let active = true;
    void listWorkers().then(
      (found) => {
        if (active) setWorkers(found);
      },
      () => undefined,
    );
    return () => {
      active = false;
    };
  }, []);

  // The skills this project offers (ADR 0053). Held with the project they
  // came from, so another project's list is never shown while the new one
  // loads, and so changing project needs no reset.
  useEffect(() => {
    if (projectId === undefined) return;
    let active = true;
    void listSkills(projectId).then(
      (found) => {
        if (active) setSkills({ projectId, skills: found });
      },
      () => {
        if (active) setSkills({ projectId, skills: [] });
      },
    );
    return () => {
      active = false;
    };
  }, [projectId]);

  // Which of them look relevant to the goal as it stands (ADR 0054). The
  // rule is the core's, asked again when the typing settles rather than on
  // every keystroke. A suggestion marks and orders; it never ticks.
  useEffect(() => {
    if (projectId === undefined) return;
    let active = true;
    const timer = setTimeout(() => {
      void suggestSkills(projectId, goal).then(
        (found) => {
          if (active) setSuggested(found);
        },
        () => {
          if (active) setSuggested([]);
        },
      );
    }, SUGGESTION_DELAY_MS);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [projectId, goal]);

  // A task runs on the worker the person picked. Under Local Only a model
  // off this device is not offered, because the core would refuse it
  // (ADR 0048).
  const offeredWorkers = useMemo(
    () => workers.filter((choice) => !(routingMode === "localOnly" && choice.cloud === true)),
    [routingMode, workers],
  );
  const chosenWorker =
    offeredWorkers.find((choice) => choice.agentId === worker) ?? offeredWorkers[0];
  // Offering is not selecting, and a tick belongs to the project it was made
  // in: only skills this project offers can reach a task, so changing project
  // drops the rest without anything having to clear them.
  const offeredSkills = useMemo(
    () => (skills !== undefined && skills.projectId === projectId ? skills.skills : []),
    [skills, projectId],
  );
  const chosenSkills = useMemo(
    () => ticked.filter((id) => offeredSkills.some((skill) => skill.id === id)),
    [ticked, offeredSkills],
  );
  // Suggested skills first, each in the order the core gave, then the rest in
  // the project's own order. Stable, so a list does not reshuffle under the
  // pointer as the goal is typed.
  const listedSkills = useMemo(
    () => [
      ...offeredSkills.filter((skill) => suggested.includes(skill.id)),
      ...offeredSkills.filter((skill) => !suggested.includes(skill.id)),
    ],
    [offeredSkills, suggested],
  );

  return {
    routingMode,
    setRoutingMode,
    admission,
    allow: (key: keyof Admission, allowed: boolean) => {
      setAdmission((current) => ({ ...current, [key]: allowed }));
    },
    offeredWorkers,
    chosenWorker,
    setWorker,
    offeredSkills,
    listedSkills,
    chosenSkills,
    suggested,
    tick: (skillId: string, chosen: boolean) => {
      setTicked((current) =>
        chosen ? [...current, skillId] : current.filter((id) => id !== skillId),
      );
    },
  };
}
