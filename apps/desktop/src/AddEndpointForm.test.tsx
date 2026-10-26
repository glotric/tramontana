import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type * as CoreModuleShape from "./core.js";

type CoreModule = typeof CoreModuleShape;

const addOpenAiEndpoint = vi.hoisted(() => vi.fn());
const addAnthropicEndpoint = vi.hoisted(() => vi.fn());
const addLlamaEndpoint = vi.hoisted(() => vi.fn());
const addMlxEndpoint = vi.hoisted(() => vi.fn());

vi.mock("./core.js", async () => {
  const actual = await vi.importActual<CoreModule>("./core.js");
  return { ...actual, addOpenAiEndpoint, addAnthropicEndpoint, addLlamaEndpoint, addMlxEndpoint };
});

const { AddEndpointForm } = await import("./AddEndpointForm.js");

describe("adding an endpoint", () => {
  beforeEach(() => {
    addLlamaEndpoint.mockResolvedValue(false);
    addMlxEndpoint.mockResolvedValue(false);
    addOpenAiEndpoint.mockResolvedValue(true);
    addAnthropicEndpoint.mockResolvedValue(true);
  });

  it("starts with a llama-server whose files come from the picker, and says it runs here", async () => {
    const onAdded = vi.fn();
    const user = userEvent.setup();
    render(<AddEndpointForm onAdded={onAdded} />);

    expect(screen.getByRole("radio", { name: "llama-server started by Tramontana" })).toBeChecked();
    expect(screen.queryByLabelText("Address")).not.toBeInTheDocument();
    const add = screen.getByRole("button", { name: "Choose files and add…" });
    expect(add).toBeDisabled();

    await user.type(screen.getByLabelText("Name"), "llama");
    await user.type(screen.getByLabelText("Model"), "qwen2.5-7b-instruct");
    await user.click(add);

    await waitFor(() => {
      expect(addLlamaEndpoint).toHaveBeenCalledWith({
        endpointId: "llama",
        modelId: "qwen2.5-7b-instruct",
      });
    });
    expect(onAdded).toHaveBeenCalledWith("Added llama. Work sent to it runs on this device.");
    expect(screen.getByLabelText("Name")).toHaveValue("");
  });

  it("adds an mlx_lm.server from picked files, says it needs Apple Silicon, and runs here", async () => {
    const onAdded = vi.fn();
    const user = userEvent.setup();
    render(<AddEndpointForm onAdded={onAdded} />);

    await user.click(
      screen.getByRole("radio", { name: "mlx_lm.server started by Tramontana (Apple Silicon)" }),
    );
    expect(screen.queryByLabelText("Address")).not.toBeInTheDocument();
    expect(screen.getByText(/MLX model folder/)).toBeInTheDocument();
    expect(screen.getByText(/only on a Mac with Apple Silicon/)).toBeInTheDocument();

    await user.type(screen.getByLabelText("Name"), "mlx");
    await user.type(screen.getByLabelText("Model"), "qwen-mlx");
    await user.click(screen.getByRole("button", { name: "Choose files and add…" }));

    await waitFor(() => {
      expect(addMlxEndpoint).toHaveBeenCalledWith({ endpointId: "mlx", modelId: "qwen-mlx" });
    });
    expect(addLlamaEndpoint).not.toHaveBeenCalled();
    expect(onAdded).toHaveBeenCalledWith("Added mlx. Work sent to it runs on this device.");
  });

  it("adds nothing, and says so, when no file is chosen", async () => {
    addLlamaEndpoint.mockResolvedValue(undefined);
    const onAdded = vi.fn();
    const user = userEvent.setup();
    render(<AddEndpointForm onAdded={onAdded} />);

    await user.type(screen.getByLabelText("Name"), "llama");
    await user.type(screen.getByLabelText("Model"), "qwen");
    await user.click(screen.getByRole("button", { name: "Choose files and add…" }));

    expect(await screen.findByRole("note")).toHaveTextContent("Nothing was added");
    expect(onAdded).not.toHaveBeenCalled();
  });

  it("adds an OpenAI-compatible server and says when its work leaves the device", async () => {
    const onAdded = vi.fn();
    const user = userEvent.setup();
    render(<AddEndpointForm onAdded={onAdded} />);

    await user.click(screen.getByRole("radio", { name: "OpenAI-compatible server" }));
    await user.type(screen.getByLabelText("Name"), "openai");
    await user.type(screen.getByLabelText("Model"), "gpt");
    await user.type(screen.getByLabelText("Address"), "https://api.openai.com");
    await user.type(screen.getByLabelText("Credential key (optional)"), "openai.apiKey");
    await user.click(screen.getByRole("button", { name: "Add endpoint" }));

    await waitFor(() => {
      expect(addOpenAiEndpoint).toHaveBeenCalledWith({
        endpointId: "openai",
        modelId: "gpt",
        baseUrl: "https://api.openai.com",
        credentialKey: "openai.apiKey",
      });
    });
    expect(onAdded).toHaveBeenCalledWith("Added openai. Work sent to it leaves this device.");
  });

  it("needs a credential key for Anthropic, at Anthropic's own address", async () => {
    const user = userEvent.setup();
    render(<AddEndpointForm onAdded={() => undefined} />);

    await user.click(screen.getByRole("radio", { name: "Anthropic" }));
    expect(screen.getByLabelText("Address")).toHaveValue("https://api.anthropic.com");
    await user.type(screen.getByLabelText("Name"), "claude");
    await user.type(screen.getByLabelText("Model"), "claude-sonnet");
    const add = screen.getByRole("button", { name: "Add endpoint" });
    expect(add).toBeDisabled();

    await user.type(screen.getByLabelText("Credential key"), "anthropic.apiKey");
    await user.click(add);
    await waitFor(() => {
      expect(addAnthropicEndpoint).toHaveBeenCalledWith({
        endpointId: "claude",
        modelId: "claude-sonnet",
        baseUrl: "https://api.anthropic.com",
        credentialKey: "anthropic.apiKey",
      });
    });
  });

  it("shows why the shell refused an endpoint, and adds nothing", async () => {
    addOpenAiEndpoint.mockRejectedValue({
      code: "invalidRequest",
      message: "plain http is accepted only for an endpoint on this device; use https",
      retryable: false,
    });
    const onAdded = vi.fn();
    const user = userEvent.setup();
    render(<AddEndpointForm onAdded={onAdded} />);

    await user.click(screen.getByRole("radio", { name: "OpenAI-compatible server" }));
    await user.type(screen.getByLabelText("Name"), "remote");
    await user.type(screen.getByLabelText("Model"), "m");
    await user.type(screen.getByLabelText("Address"), "http://models.example.com");
    await user.click(screen.getByRole("button", { name: "Add endpoint" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("plain http is accepted only");
    expect(onAdded).not.toHaveBeenCalled();
  });
});
