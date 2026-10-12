import { useCallback, useEffect, useRef, useState } from "react";
import type { ConversationRecord, ProjectRecord, TaskRecord } from "@tramontana/shared";

import { failureText, inspectProject, listConversations, listProjects, listTasks } from "./core.js";
import type { ProjectInspection } from "./core.js";
import type { TaskStream } from "./useTaskStream.js";

/**
 * The project, conversation and task on screen, each chosen after the one
 * above it.
 *
 * Choosing anew drops whatever is still loading for the choice before: each
 * level has a request counter, and a choice at one level also moves every
 * level below it on. `moves` counts the person's own choices, so work
 * started before one — a task's start — can tell it no longer has the
 * screen.
 */
export function useProjectSelection(
  task: Pick<TaskStream, "select" | "clear">,
  onFailure: (text: string | undefined) => void,
) {
  const [projects, setProjects] = useState<readonly ProjectRecord[]>([]);
  const [inspection, setInspection] = useState<ProjectInspection | undefined>();
  const [conversations, setConversations] = useState<readonly ConversationRecord[]>([]);
  const [selectedConversationId, setSelectedConversationId] = useState<string | undefined>();
  const [tasks, setTasks] = useState<readonly TaskRecord[]>([]);
  const [loadingProject, setLoadingProject] = useState(false);
  const projectRequest = useRef(0);
  const conversationRequest = useRef(0);
  const moves = useRef(0);
  // What is on screen now, for answers that arrive after a change of it.
  const shownConversation = useRef<string | undefined>(undefined);
  const shownProject = useRef<string | undefined>(undefined);

  const showConversation = useCallback((conversationId: string | undefined): void => {
    shownConversation.current = conversationId;
    setSelectedConversationId(conversationId);
  }, []);
  const { select: selectTask, clear: clearTask } = task;

  const selectConversation = useCallback(
    async (conversationId: string): Promise<void> => {
      const request = ++conversationRequest.current;
      moves.current += 1;
      clearTask();
      showConversation(conversationId);
      setTasks([]);
      onFailure(undefined);
      try {
        const records = await listTasks(conversationId);
        if (request !== conversationRequest.current) return;
        setTasks(records);
        const latest = records[0];
        if (latest !== undefined) await selectTask(latest.taskId);
      } catch (error: unknown) {
        if (request === conversationRequest.current) {
          onFailure(failureText(error, "Could not load conversation tasks."));
        }
      }
    },
    [clearTask, onFailure, selectTask, showConversation],
  );

  /** Back to the project's home: no conversation open, the project kept. */
  const leaveConversation = useCallback(() => {
    conversationRequest.current += 1;
    moves.current += 1;
    clearTask();
    showConversation(undefined);
    setTasks([]);
  }, [clearTask, showConversation]);

  /** Show no project: nothing of the one before stays on screen. */
  const closeProject = useCallback(() => {
    projectRequest.current += 1;
    conversationRequest.current += 1;
    moves.current += 1;
    shownProject.current = undefined;
    clearTask();
    setInspection(undefined);
    setConversations([]);
    showConversation(undefined);
    setTasks([]);
  }, [clearTask, showConversation]);

  const selectProject = useCallback(
    async (projectId: string): Promise<void> => {
      closeProject();
      const request = projectRequest.current;
      setLoadingProject(true);
      onFailure(undefined);
      try {
        const [detail, records] = await Promise.all([
          inspectProject(projectId),
          listConversations(projectId),
        ]);
        if (request !== projectRequest.current) return;
        shownProject.current = projectId;
        setInspection(detail);
        setConversations(records);
        // The project is ready for a new task; a conversation opens only
        // when the person picks one.
      } catch (error: unknown) {
        if (request === projectRequest.current) {
          onFailure(failureText(error, "Could not load the selected project."));
        }
      } finally {
        if (request === projectRequest.current) setLoadingProject(false);
      }
    },
    [closeProject, onFailure],
  );

  // The recent projects, opening the latest.
  useEffect(() => {
    let active = true;
    void listProjects().then(
      (records) => {
        if (!active) return;
        setProjects(records);
        const latest = records[0];
        if (latest !== undefined) void selectProject(latest.projectId);
      },
      (error: unknown) => {
        if (active) onFailure(failureText(error, "Could not load recent projects."));
      },
    );
    return () => {
      active = false;
      projectRequest.current += 1;
      conversationRequest.current += 1;
    };
  }, [onFailure, selectProject]);

  /**
   * Show a conversation created a moment ago, with no tasks to read yet:
   * the task about to start in it is already on its way to the screen.
   *
   * Only into the project it belongs to, while that project is on screen:
   * a conversation created as the person moved to another project is not
   * put into that one's list. Says whether it was shown.
   */
  const addConversation = useCallback(
    (created: ConversationRecord): boolean => {
      if (created.projectId === undefined || created.projectId !== shownProject.current) {
        return false;
      }
      conversationRequest.current += 1;
      setConversations((current) => [created, ...current]);
      showConversation(created.conversationId);
      setTasks([]);
      return true;
    },
    [showConversation],
  );

  /**
   * Read a conversation's tasks again. The answer is shown only if that
   * conversation is still the one on screen when it arrives: a slow read
   * must not put one conversation's tasks under another's title.
   */
  const refreshTasks = useCallback((conversationId?: string) => {
    const id = conversationId ?? shownConversation.current;
    if (id === undefined) return;
    void listTasks(id).then(
      (records) => {
        if (shownConversation.current === id) setTasks(records);
      },
      () => undefined,
    );
  }, []);

  /** How many choices the person has made: compare two readings to tell. */
  const moveCount = useCallback(() => moves.current, []);

  return {
    projects,
    setProjects,
    inspection,
    setInspection,
    conversations,
    setConversations,
    selectedConversationId,
    tasks,
    loadingProject,
    selectProject,
    selectConversation,
    closeProject,
    leaveConversation,
    addConversation,
    refreshTasks,
    moveCount,
  };
}
