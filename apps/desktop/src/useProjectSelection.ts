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
 * level below it on.
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
  const { select: selectTask, clear: clearTask } = task;

  const selectConversation = useCallback(
    async (conversationId: string): Promise<void> => {
      const request = ++conversationRequest.current;
      clearTask();
      setSelectedConversationId(conversationId);
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
    [clearTask, onFailure, selectTask],
  );

  /** Show no project: nothing of the one before stays on screen. */
  const closeProject = useCallback(() => {
    projectRequest.current += 1;
    conversationRequest.current += 1;
    clearTask();
    setInspection(undefined);
    setConversations([]);
    setSelectedConversationId(undefined);
    setTasks([]);
  }, [clearTask]);

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
        setInspection(detail);
        setConversations(records);
        const latest = records[0];
        if (latest !== undefined) await selectConversation(latest.conversationId);
      } catch (error: unknown) {
        if (request === projectRequest.current) {
          onFailure(failureText(error, "Could not load the selected project."));
        }
      } finally {
        if (request === projectRequest.current) setLoadingProject(false);
      }
    },
    [closeProject, onFailure, selectConversation],
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

  const refreshTasks = useCallback(() => {
    if (selectedConversationId === undefined) return;
    void listTasks(selectedConversationId).then(setTasks, () => undefined);
  }, [selectedConversationId]);

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
    refreshTasks,
  };
}
