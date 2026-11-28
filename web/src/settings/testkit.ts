// Test helpers shared by the settings screens' tests: a scripted service that
// records every call, and the fixtures its answers are made of. Nothing here
// touches the network or the real HOME.

import { ApiClient, type FetchLike } from "../api/client";
import type {
  PromptDto,
  PromptListDto,
  RosterDto,
  SessionListDto,
  SettingsDto,
  SettingsValueDto,
  TunnelStatusDto,
} from "../gen";

/** One call the page made. */
export interface Call {
  /** The HTTP method. */
  method: string;
  /** Path and query. */
  url: string;
  /** The JSON body, when there was one. */
  body: unknown;
}

/** A route's answer: a Response, or a function of the call. */
export type Answer = Response | ((call: Call) => Response | Promise<Response>);

/** A JSON answer with the service's content type. */
export function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** A refusal in the service's own shape. */
export function refusal(status: number, code: string, message: string): Response {
  return json(status, { code, message });
}

/**
 * A client over scripted routes, keyed `METHOD /path`. A route that is not
 * scripted answers `404`, which is what a service without it would do.
 */
export function scripted(routes: Record<string, Answer>): { api: ApiClient; calls: Call[] } {
  const calls: Call[] = [];
  const fetch: FetchLike = async (url, init) => {
    const method = init?.method ?? "GET";
    const body = typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : undefined;
    const call = { method, url, body };
    calls.push(call);
    const path = url.split("?")[0] ?? url;
    const answer = routes[`${method} ${url}`] ?? routes[`${method} ${path}`];
    if (answer === undefined) {
      return refusal(404, "not_found", "no such route");
    }
    return typeof answer === "function" ? await answer(call) : answer.clone();
  };
  return { api: new ApiClient({ fetch }), calls };
}

/** One settings row. */
export function row(
  key: string,
  value: SettingsValueDto["value"],
  source: SettingsValueDto["source"] = "built_in",
): SettingsValueDto {
  return { key, value, source };
}

/** The settings a fresh install resolves: three roles and the workflow keys. */
export function settings(overrides: SettingsValueDto[] = [], extra: Partial<SettingsDto> = {}): SettingsDto {
  const base: SettingsValueDto[] = [
    row("roles.order", ["coordinator", "worker", "reviewer"]),
    row("roles.worker.title", "Worker"),
    row("roles.worker.about", "Does the task."),
    row("roles.worker.kind", "worker"),
    row("roles.worker.prompt", "prompt:worker"),
    row("roles.worker.agent", "claude", "you"),
    row("roles.worker.model", "sonnet", "you"),
    row("roles.worker.effort", null, null),
    row("roles.reviewer.title", "Reviewer"),
    row("roles.reviewer.kind", "reviewer"),
    row("roles.reviewer.prompt", "prompt:reviewer"),
    row("roles.reviewer.readonly", true),
    row("roles.reviewer.agent", "codex", "you"),
    row("tasks.merge", "ask"),
    row("tasks.review_rounds", 3),
    row("tasks.must_fix", "major"),
    row("tasks.proof", true),
    row("tasks.max_running", 4),
    row("tasks.archive_namespace", ""),
    row("tasks.start_notes", "Be kind."),
    row("review.cross_vendor", true),
  ];
  const keys = new Set(overrides.map((entry) => entry.key));
  return {
    repo: "shprd",
    values: [...base.filter((entry) => !keys.has(entry.key)), ...overrides],
    problems: [],
    ...extra,
  };
}

/** A roster with two programs. */
export const ROSTER: RosterDto = {
  programs: [
    {
      id: "claude",
      name: "Claude Code",
      installed: true,
      vendor: "anthropic",
      multi_model: false,
      needs_provider: false,
      models: [
        { id: "sonnet", name: "Sonnet", efforts: ["low", "high"], fast: false },
        { id: "opus", name: "opus", efforts: ["high"], fast: false },
      ],
      models_source: "static",
      efforts: ["low", "high"],
      fast: false,
      permissions: [],
    },
    {
      id: "codex",
      name: "Codex",
      installed: true,
      vendor: "openai",
      multi_model: false,
      needs_provider: false,
      models: [],
      models_source: "none",
      models_note: "Codex lists no models; type an id.",
      efforts: [],
      fast: false,
      permissions: [],
    },
  ],
};

/** A prompt list with one shipped and one edited prompt. */
export const PROMPTS: PromptListDto = {
  prompts: [
    { id: "worker", about: "What a worker starts with.", path: "/p/worker.md", state: "default", shipped: true },
    { id: "reviewer", about: "What a reviewer starts with.", path: "/p/reviewer.md", state: "edited", shipped: true },
  ],
};

/** One prompt. */
export function prompt(id: string, text: string, state: PromptDto["state"] = "default"): PromptDto {
  return { id, path: `/p/${id}.md`, text, state };
}

/** A tunnel that is off. */
export function tunnelOff(extra: Partial<TunnelStatusDto> = {}): TunnelStatusDto {
  return {
    on: false,
    starting: false,
    at: 0,
    password_set: true,
    default_provider: "tailscale",
    providers: [
      { id: "tailscale", public_internet: false },
      { id: "cloudflared", public_internet: true },
    ],
    ...extra,
  };
}

/** A tunnel that is up. */
export function tunnelOn(extra: Partial<TunnelStatusDto> = {}): TunnelStatusDto {
  return tunnelOff({
    on: true,
    provider: "cloudflared",
    link: "https://quiet-river-1234.trycloudflare.com",
    qr_svg: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1"/></svg>',
    ...extra,
  });
}

/** Two sessions, the second one this device. */
export const SESSIONS: SessionListDto = {
  current: "bbbbbbbbbbbb",
  sessions: [
    {
      id: "aaaaaaaaaaaa",
      created: 1,
      last_seen: 1,
      fresh_until: 1,
      fresh: false,
      remember: true,
      ua_class: "phone",
      label: "phone · Safari",
      expires_at: 99,
    },
    {
      id: "bbbbbbbbbbbb",
      created: 1,
      last_seen: Math.floor(Date.now() / 1000),
      fresh_until: 1,
      fresh: true,
      remember: false,
      ua_class: "desktop",
      label: "desktop · Firefox",
      expires_at: 99,
    },
  ],
};

/** The three reads every page of the dialog starts with. */
export function reads(overrides: Record<string, Answer> = {}): Record<string, Answer> {
  return {
    "GET /api/settings": json(200, settings()),
    "GET /api/roster": json(200, ROSTER),
    "GET /api/prompts": json(200, PROMPTS),
    ...overrides,
  };
}
