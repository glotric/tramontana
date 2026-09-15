import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const Channel = vi.hoisted(() =>
  vi.fn(function (this: { onmessage?: unknown }) {
    this.onmessage = undefined;
  }),
);
vi.mock("@tauri-apps/api/core", () => ({ invoke, Channel }));
const listen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/event", () => ({ listen }));

const {
  addAnthropicEndpoint,
  addLlamaEndpoint,
  addOpenAiEndpoint,
  asErrorEnvelope,
  cancelTask,
  createConversation,
  exportBundle,
  fetchCoreInfo,
  forgetCredential,
  importStagedBundle,
  inspectBundle,
  listConfiguredEndpoints,
  listModelEndpoints,
  removeEndpoint,
  restartApplication,
  setEndpointEnabled,
  setEndpointToolCalls,
  storeCredential,
  getTask,
  inspectProject,
  listConversations,
  listPendingApprovals,
  listProjects,
  listTasks,
  onApprovalRequested,
  openProject,
  projectChanges,
  replayTaskEvents,
  resolveApproval,
  startTask,
} = await import("./core.js");

describe("approval IPC", () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockReset();
  });

  it("asks for the questions still waiting and answers one by identifier", async () => {
    invoke.mockResolvedValue([]);
    await listPendingApprovals();
    expect(invoke).toHaveBeenCalledWith("pending_approvals");

    invoke.mockResolvedValue(undefined);
    await resolveApproval("approval-1", "allowOnce");
    expect(invoke).toHaveBeenCalledWith("resolve_approval", {
      approvalId: "approval-1",
      decision: "allowOnce",
    });
  });

  it("delivers each question the shell raises, until told to stop", async () => {
    const unlisten = vi.fn();
    let deliver: (event: { payload: unknown }) => void = () => undefined;
    listen.mockImplementation((name: string, handler: (event: { payload: unknown }) => void) => {
      // The name `approval.rs` emits under; nothing at compile time links them.
      expect(name).toBe("tramontana://approval-requested");
      deliver = handler;
      return Promise.resolve(unlisten);
    });

    const seen: unknown[] = [];
    const stop = await onApprovalRequested((request) => {
      seen.push(request);
    });
    deliver({ payload: { approvalId: "approval-1" } });
    expect(seen).toEqual([{ approvalId: "approval-1" }]);

    stop();
    expect(unlisten).toHaveBeenCalledOnce();
  });
});

describe("models and credentials", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
  });

  it("invokes the settings commands the shell registers", async () => {
    invoke.mockResolvedValue([]);
    await listConfiguredEndpoints();
    expect(invoke).toHaveBeenLastCalledWith("list_configured_endpoints");
    await listModelEndpoints();
    expect(invoke).toHaveBeenLastCalledWith("list_model_endpoints");

    invoke.mockResolvedValue(undefined);
    await setEndpointEnabled("local", false);
    expect(invoke).toHaveBeenLastCalledWith("set_endpoint_enabled", {
      endpointId: "local",
      enabled: false,
    });
    await setEndpointToolCalls("local", true);
    expect(invoke).toHaveBeenLastCalledWith("set_endpoint_tool_calls", {
      endpointId: "local",
      supported: true,
    });
    await removeEndpoint("local");
    expect(invoke).toHaveBeenLastCalledWith("remove_endpoint", { endpointId: "local" });

    invoke.mockResolvedValue(7);
    expect(await storeCredential("openai.apiKey", "sk-test")).toBe(7);
    expect(invoke).toHaveBeenLastCalledWith("store_credential", {
      key: "openai.apiKey",
      value: "sk-test",
    });
    invoke.mockResolvedValue(true);
    await forgetCredential("openai.apiKey");
    expect(invoke).toHaveBeenLastCalledWith("forget_credential", { key: "openai.apiKey" });

    invoke.mockResolvedValue(undefined);
    await restartApplication();
    expect(invoke).toHaveBeenLastCalledWith("restart_application");
  });

  it("adds each kind of endpoint by name, and never with a path", async () => {
    invoke.mockResolvedValue(false);
    await addOpenAiEndpoint({
      endpointId: "local",
      baseUrl: "http://127.0.0.1:8080",
      modelId: "qwen",
    });
    expect(invoke).toHaveBeenLastCalledWith("add_openai_endpoint", {
      endpointId: "local",
      displayName: null,
      baseUrl: "http://127.0.0.1:8080",
      modelId: "qwen",
      credentialKey: null,
    });

    invoke.mockResolvedValue(true);
    await addAnthropicEndpoint({
      endpointId: "claude",
      displayName: "Claude",
      baseUrl: "https://api.anthropic.com",
      modelId: "claude",
      credentialKey: "anthropic.apiKey",
    });
    expect(invoke).toHaveBeenLastCalledWith("add_anthropic_endpoint", {
      endpointId: "claude",
      displayName: "Claude",
      baseUrl: "https://api.anthropic.com",
      modelId: "claude",
      credentialKey: "anthropic.apiKey",
    });

    invoke.mockResolvedValue(null);
    expect(await addLlamaEndpoint({ endpointId: "llama", modelId: "qwen" })).toBeUndefined();
    expect(invoke).toHaveBeenLastCalledWith("add_llama_endpoint", {
      endpointId: "llama",
      displayName: null,
      modelId: "qwen",
    });
  });
});

describe("bundles", () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it("names what to export, and never a folder", async () => {
    invoke.mockResolvedValue(null);
    expect(
      await exportBundle(
        { projectId: "project-1" },
        { includeAbsolutePaths: false, includeRawToolOutput: false },
      ),
    ).toBeUndefined();
    expect(invoke).toHaveBeenLastCalledWith("export_bundle", {
      taskId: null,
      projectId: "project-1",
      includeAbsolutePaths: false,
      includeRawToolOutput: false,
    });

    await exportBundle(
      { taskId: "task-1" },
      { includeAbsolutePaths: true, includeRawToolOutput: true },
    );
    expect(invoke).toHaveBeenLastCalledWith("export_bundle", {
      taskId: "task-1",
      projectId: null,
      includeAbsolutePaths: true,
      includeRawToolOutput: true,
    });
  });

  it("inspects a picked bundle, then imports it by identifier", async () => {
    invoke.mockResolvedValue(null);
    expect(await inspectBundle()).toBeUndefined();
    expect(invoke).toHaveBeenLastCalledWith("inspect_bundle");

    invoke.mockResolvedValue({ schemaVersion: 1, imported: {}, executedAnything: false });
    await importStagedBundle("bundle-1");
    expect(invoke).toHaveBeenLastCalledWith("import_staged_bundle", { stagingId: "bundle-1" });
  });
});

describe("project changes", () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it("asks the shell for the project's status and diff by project", async () => {
    invoke.mockResolvedValue({ schemaVersion: 1, status: "", diff: "", truncated: false });
    await projectChanges("project-1");
    expect(invoke).toHaveBeenCalledWith("project_changes", { projectId: "project-1" });
  });
});

describe("task options", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue("task-1");
  });

  it("sends what the person chose, and nothing when they chose nothing", async () => {
    await startTask("hello", () => undefined, "conversation-1", {
      writes: true,
      recipes: false,
      routingMode: "localOnly",
    });
    expect(invoke).toHaveBeenLastCalledWith(
      "start_task",
      expect.objectContaining({
        admission: { writes: true, recipes: false },
        routingMode: "localOnly",
      }),
    );

    await startTask("hello", () => undefined);
    expect(invoke).toHaveBeenLastCalledWith(
      "start_task",
      expect.objectContaining({ admission: null, routingMode: null }),
    );
  });
});

describe("core IPC client", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue({ schemaVersion: 1, appVersion: "0.1.0" });
  });

  it("invokes the command names the Rust shell registers", async () => {
    // Command names are strings here and identifiers in Rust, so nothing at
    // compile time links them. `tauri::generate_handler!` registers these;
    // a rename on either side must fail here rather than at runtime as a silent
    // "core unavailable".
    await fetchCoreInfo();
    expect(invoke).toHaveBeenCalledWith("core_info");

    invoke.mockResolvedValue(null);
    await openProject();
    expect(invoke).toHaveBeenCalledWith("open_project");

    invoke.mockResolvedValue([]);
    await listProjects();
    expect(invoke).toHaveBeenCalledWith("list_projects");

    invoke.mockResolvedValue({});
    await inspectProject("project-1");
    expect(invoke).toHaveBeenCalledWith("inspect_project", { projectId: "project-1" });

    invoke.mockResolvedValue({});
    await createConversation("project-1", "M2");
    expect(invoke).toHaveBeenCalledWith("create_conversation", {
      projectId: "project-1",
      title: "M2",
    });

    invoke.mockResolvedValue([]);
    await listConversations("project-1");
    expect(invoke).toHaveBeenCalledWith("list_conversations", { projectId: "project-1" });

    await listTasks("conversation-1");
    expect(invoke).toHaveBeenCalledWith("list_tasks", { conversationId: "conversation-1" });

    await replayTaskEvents("task-1");
    expect(invoke).toHaveBeenCalledWith("replay_task_events", { taskId: "task-1" });

    invoke.mockResolvedValue("task-1");
    await startTask("hello", () => undefined, "conversation-1");
    expect(invoke).toHaveBeenCalledWith(
      "start_task",
      expect.objectContaining({ goal: "hello", conversationId: "conversation-1" }),
    );

    invoke.mockResolvedValue(undefined);
    await cancelTask("task-1");
    expect(invoke).toHaveBeenCalledWith("cancel_task", { taskId: "task-1" });

    invoke.mockResolvedValue({
      schemaVersion: 1,
      taskId: "task-1",
      state: "queued",
      lastSequence: 0,
    });
    await getTask("task-1");
    expect(invoke).toHaveBeenCalledWith("get_task", { taskId: "task-1" });
  });

  it("wires the update callback to the channel it passes across the boundary", async () => {
    invoke.mockResolvedValue("task-1");
    const onUpdate = vi.fn();
    await startTask("hello", onUpdate);

    const payload = invoke.mock.calls.at(-1)?.[1] as {
      channel: { onmessage: (u: unknown) => void };
    };
    const channel = payload.channel;
    channel.onmessage({ kind: "frame", frame: { streamId: "s", offset: 0, text: "hi" } });
    expect(onUpdate).toHaveBeenCalledWith({
      kind: "frame",
      frame: { streamId: "s", offset: 0, text: "hi" },
    });
  });

  it("recognizes a normalized error and rejects anything else", () => {
    expect(
      asErrorEnvelope({ code: "notFound", message: "unknown task", retryable: false }),
    ).toMatchObject({ code: "notFound" });

    // A thrown string or a plain Error is not a contract error; treating one as
    // if it were would put unredacted text in front of the user.
    expect(asErrorEnvelope("boom")).toBeUndefined();
    expect(asErrorEnvelope(new Error("boom"))).toBeUndefined();
    expect(asErrorEnvelope(null)).toBeUndefined();
  });
});
