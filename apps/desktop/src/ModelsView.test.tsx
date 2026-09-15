import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type * as CoreModuleShape from "./core.js";
import type { ConfiguredEndpoint, ModelEndpointStatus } from "./core.js";

type CoreModule = typeof CoreModuleShape;

const listConfiguredEndpoints = vi.hoisted(() => vi.fn());
const listModelEndpoints = vi.hoisted(() => vi.fn());
const setEndpointEnabled = vi.hoisted(() => vi.fn());
const setEndpointToolCalls = vi.hoisted(() => vi.fn());
const removeEndpoint = vi.hoisted(() => vi.fn());
const storeCredential = vi.hoisted(() => vi.fn());
const forgetCredential = vi.hoisted(() => vi.fn());
const restartApplication = vi.hoisted(() => vi.fn());
const addLlamaEndpoint = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return {
    ...actual,
    listConfiguredEndpoints,
    listModelEndpoints,
    setEndpointEnabled,
    setEndpointToolCalls,
    removeEndpoint,
    storeCredential,
    forgetCredential,
    restartApplication,
    addLlamaEndpoint,
  };
});

const { ModelsView } = await import("./ModelsView.js");

const llama: ConfiguredEndpoint = {
  endpointId: "llama",
  displayName: "Local Qwen",
  kind: "llamaServer",
  modelId: "qwen2.5-7b-instruct",
  leavesTheDevice: false,
  enabled: true,
  supportsToolCalls: true,
};

const openai: ConfiguredEndpoint = {
  endpointId: "openai",
  displayName: "OpenAI",
  kind: "openAiCompatible",
  modelId: "gpt",
  leavesTheDevice: true,
  enabled: true,
  supportsToolCalls: false,
  credential: { key: "openai.apiKey", configured: false },
};

const added: ConfiguredEndpoint = {
  endpointId: "later",
  displayName: "Added since opening",
  kind: "openAiCompatible",
  modelId: "m",
  leavesTheDevice: false,
  enabled: true,
  supportsToolCalls: false,
};

const off: ConfiguredEndpoint = { ...added, endpointId: "off", displayName: "Off", enabled: false };

const health: ModelEndpointStatus[] = [
  { endpointId: "llama", models: ["qwen2.5-7b-instruct"], leavesTheDevice: false, available: true },
  {
    endpointId: "openai",
    models: [],
    leavesTheDevice: false,
    available: false,
    unavailableReason: "no credential is stored for openai.apiKey",
  },
];

function endpoint(name: string): HTMLElement {
  const item = screen
    .getAllByRole("listitem")
    .find((candidate) => within(candidate).queryByRole("heading", { name }) !== null);
  if (item === undefined) throw new Error(`no endpoint named ${name}`);
  return item;
}

describe("the Models view", () => {
  beforeEach(() => {
    for (const mock of [
      listConfiguredEndpoints,
      listModelEndpoints,
      setEndpointEnabled,
      setEndpointToolCalls,
      removeEndpoint,
      storeCredential,
      forgetCredential,
      restartApplication,
      addLlamaEndpoint,
    ]) {
      mock.mockReset();
    }
    addLlamaEndpoint.mockResolvedValue(false);
    listConfiguredEndpoints.mockResolvedValue([llama, openai, added, off]);
    listModelEndpoints.mockResolvedValue(health);
    setEndpointEnabled.mockResolvedValue(undefined);
    setEndpointToolCalls.mockResolvedValue(undefined);
    removeEndpoint.mockResolvedValue(undefined);
    storeCredential.mockResolvedValue(7);
    forgetCredential.mockResolvedValue(true);
    restartApplication.mockResolvedValue(undefined);
  });

  it("says where each endpoint runs and whether it is in use, in words", async () => {
    render(<ModelsView onClose={() => undefined} />);
    await screen.findByRole("heading", { name: "Local Qwen" });

    expect(endpoint("Local Qwen")).toHaveTextContent("Runs on this device");
    expect(endpoint("Local Qwen")).toHaveTextContent("In use · qwen2.5-7b-instruct");
    expect(endpoint("OpenAI")).toHaveTextContent("Leaves this device");
    expect(endpoint("OpenAI")).toHaveTextContent(
      "Unavailable: no credential is stored for openai.apiKey",
    );
    expect(endpoint("Added since opening")).toHaveTextContent(
      "Not in use until Tramontana restarts",
    );
    expect(endpoint("Off")).toHaveTextContent("Turned off");
  });

  it("saves a switch at once, and offers the restart that puts it to use", async () => {
    const user = userEvent.setup();
    render(<ModelsView onClose={() => undefined} />);
    await screen.findByRole("heading", { name: "Local Qwen" });

    await user.click(within(endpoint("Local Qwen")).getByRole("checkbox", { name: "Enabled" }));
    await waitFor(() => {
      expect(setEndpointEnabled).toHaveBeenCalledWith("llama", false);
    });
    await user.click(
      within(endpoint("OpenAI")).getByRole("checkbox", { name: "Tool calls verified" }),
    );
    expect(setEndpointToolCalls).toHaveBeenCalledWith("openai", true);

    const restart = await screen.findByRole("note");
    expect(restart).toHaveTextContent("used once Tramontana restarts");
    expect(restartApplication).not.toHaveBeenCalled();
    await user.click(within(restart).getByRole("button", { name: "Restart now" }));
    expect(restartApplication).toHaveBeenCalledOnce();
  });

  it("stores a typed credential, clears it, and shows only its length", async () => {
    const user = userEvent.setup();
    render(<ModelsView onClose={() => undefined} />);
    await screen.findByRole("heading", { name: "OpenAI" });

    const card = endpoint("OpenAI");
    expect(card).toHaveTextContent("Credential openai.apiKey: not stored");
    const field = within(card).getByLabelText("New value for openai.apiKey");
    expect(field).toHaveAttribute("type", "password");
    await user.type(field, "sk-test");
    await user.click(within(card).getByRole("button", { name: "Save credential" }));

    await waitFor(() => {
      expect(storeCredential).toHaveBeenCalledWith("openai.apiKey", "sk-test");
    });
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Stored 7 characters for openai.apiKey.",
    );
    expect(field).toHaveValue("");
    expect(document.body).not.toHaveTextContent("sk-test");
  });

  it("says a store that could not answer could not answer, and forgets on request", async () => {
    listConfiguredEndpoints.mockResolvedValue([
      {
        ...openai,
        credential: {
          key: "openai.apiKey",
          unavailableReason: "the credential store is unavailable",
        },
      },
    ]);
    const user = userEvent.setup();
    render(<ModelsView onClose={() => undefined} />);
    await screen.findByRole("heading", { name: "OpenAI" });

    expect(endpoint("OpenAI")).toHaveTextContent(
      "could not be checked: the credential store is unavailable",
    );
    expect(endpoint("OpenAI")).not.toHaveTextContent("not stored");
    await user.click(screen.getByRole("button", { name: "Forget credential" }));
    await waitFor(() => {
      expect(forgetCredential).toHaveBeenCalledWith("openai.apiKey");
    });
  });

  it("says what was added and where it runs, reads the list again, and offers the restart", async () => {
    const user = userEvent.setup();
    render(<ModelsView onClose={() => undefined} />);
    await screen.findByRole("heading", { name: "Local Qwen" });
    expect(listConfiguredEndpoints).toHaveBeenCalledTimes(1);

    const form = screen.getByRole("form", { name: "Add an endpoint" });
    await user.type(within(form).getByLabelText("Name"), "second");
    await user.type(within(form).getByLabelText("Model"), "qwen");
    await user.click(within(form).getByRole("button", { name: "Choose files and add…" }));

    expect(await screen.findByRole("status")).toHaveTextContent(
      "Added second. Work sent to it runs on this device.",
    );
    expect(screen.getByRole("note")).toHaveTextContent("used once Tramontana restarts");
    await waitFor(() => {
      expect(listConfiguredEndpoints).toHaveBeenCalledTimes(2);
    });
  });

  it("asks before removing an endpoint", async () => {
    const user = userEvent.setup();
    render(<ModelsView onClose={() => undefined} />);
    await screen.findByRole("heading", { name: "Local Qwen" });

    await user.click(within(endpoint("Local Qwen")).getByRole("button", { name: "Remove…" }));
    expect(removeEndpoint).not.toHaveBeenCalled();
    await user.click(within(endpoint("Local Qwen")).getByRole("button", { name: "Keep" }));
    await user.click(within(endpoint("Local Qwen")).getByRole("button", { name: "Remove…" }));
    await user.click(within(endpoint("Local Qwen")).getByRole("button", { name: "Remove" }));
    await waitFor(() => {
      expect(removeEndpoint).toHaveBeenCalledOnce();
    });
    expect(removeEndpoint).toHaveBeenCalledWith("llama");
  });
});
