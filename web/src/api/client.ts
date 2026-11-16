// The one place the web app talks to `shprd serve`.
//
// Every request goes through here so three things cannot be forgotten:
//
// - `credentials: "same-origin"`, which is what sends the session cookie; the
//   service never accepts a page without it (M11.1's gate);
// - `x-shprd: 1` on every write, the same header the Herdr mods send, so a
//   cross-site form cannot post one;
// - a `401` puts the app back on the login screen, and a `{code: "reauth"}`
//   answer asks for the password again rather than pretending the session is
//   still fresh.
//
// The request and answer shapes come from `web/src/gen/`, which is generated
// from the service's DTOs (`cargo run -p shprd-server --example export-ts`).
// Nothing here hand-writes a shape the service already owns.

import type {
  Action,
  ActionReply,
  AnswerAsk,
  AskListDto,
  AskReply,
  ChangeDto,
  ChatDto,
  ChatSendDto,
  ChatSendReply,
  ClientMessage,
  GraphDto,
  NewTaskDto,
  PromptDto,
  PromptListDto,
  PromptPutDto,
  RosterDto,
  SettingsDto,
  SettingsPutDto,
  SettingsWriteDto,
  SnapshotDto,
  TaskChangesDto,
  WsMessage,
} from "../gen";

/** The header every write carries; the service refuses one without it. */
export const WRITE_HEADER = "x-shprd";

/**
 * The login contract this app codes against.
 *
 * `POST /api/login` and `GET /api/session` are built by M11.1 in `shprd-remote`.
 * Until its DTOs are generated into `web/src/gen/`, these three shapes are the
 * contract as this task was given it; they are structural, so a client can keep
 * them when the generated types land.
 */

/** What `GET /api/session` answers when a session is live. */
export interface SessionInfo {
  /** When the password must be entered again, ms since the Unix epoch. */
  fresh_until: number;
  /** Whether this session is a "remember this device" one. */
  remember: boolean;
}

/** The body of `POST /api/login`. */
export interface LoginRequest {
  /** The person's password. Never logged, never stored. */
  password: string;
  /** Whether the session should outlive a browser restart. */
  remember: boolean;
}

/** The body of a refusal that needs the password again. */
export interface ReauthBody {
  /** Always `reauth`. */
  code: "reauth";
  /** One line for the person. */
  message: string;
}

/**
 * A refusal, typed.
 *
 * `status` is the HTTP status, `code` the service's own word (or `network` when
 * the request never arrived), and `retryAfter` the seconds a `429` asks for.
 */
export class ApiError extends Error {
  /** The HTTP status, or `0` when the request never reached the service. */
  readonly status: number;
  /** The service's code, or `network` / `unsupported` for local failures. */
  readonly code: string;
  /** The `Retry-After` a rate limit named, in seconds. */
  readonly retryAfter?: number;

  /** Builds one refusal. */
  constructor(status: number, code: string, message: string, retryAfter?: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    if (retryAfter !== undefined) {
      this.retryAfter = retryAfter;
    }
  }

  /** Whether this refusal is the login screen's business. */
  get signsOut(): boolean {
    return this.status === 401 && this.code !== "rate_limited";
  }
}

/** What a caller may hand the client; everything has a production default. */
export interface ClientOptions {
  /** Origin prefix, `""` for the page's own origin. */
  base?: string;
  /** `fetch`, injectable so a test never touches the network. */
  fetch?: FetchLike;
  /** `WebSocket`, injectable for the same reason. */
  WebSocket?: WebSocketLikeCtor;
}

/** The slice of `fetch` the client uses. */
export type FetchLike = (
  input: string,
  init?: RequestInit,
) => Promise<Response>;

/** One socket message handler set; unknown message types are ignored. */
export interface SocketHandlers {
  /** The service introduced itself. */
  hello?: (hello: Extract<WsMessage, { type: "hello" }>) => void;
  /** Something in the run changed. */
  change?: (change: ChangeDto) => void;
  /** The socket opened. */
  open?: () => void;
  /** The socket closed, for whatever reason. */
  close?: () => void;
}

/** The client's view of one socket. */
export interface Socket {
  /** Sends one message, when the socket is open. */
  send(message: ClientMessage): void;
  /** Closes the socket; safe to call twice. */
  close(): void;
}

/** The subset of `WebSocket` the client drives. */
export interface WebSocketLike {
  /** Called when the socket opens. */
  onopen: (() => void) | null;
  /** Called for every frame the service sends. */
  onmessage: ((event: { data: unknown }) => void) | null;
  /** Called when the socket closes. */
  onclose: (() => void) | null;
  /** Called when the socket cannot connect. */
  onerror: (() => void) | null;
  /** Sends one frame. */
  send(data: string): void;
  /** Closes the socket. */
  close(): void;
}

/** The constructor shape a `WebSocketLike` factory has. */
export type WebSocketLikeCtor = new (url: string) => WebSocketLike;

/** What `GET /api/changes` answers with when nothing changed, as `null`. */
const NO_CHANGE = 204;

/** How long a change long-poll waits when the caller says nothing. */
export const DEFAULT_CHANGE_WAIT_MS = 10_000;

/**
 * One client for one service.
 *
 * Build it once per page and pass it down; a component that constructs one per
 * render would open a socket per render.
 */
export class ApiClient {
  readonly #base: string;
  readonly #fetch: FetchLike;
  readonly #WebSocket: WebSocketLikeCtor | undefined;
  readonly #unauthorized = new Set<() => void>();
  readonly #reauth = new Set<() => void>();

  /** Builds a client; the defaults are the browser's own `fetch`/`WebSocket`. */
  constructor(options: ClientOptions = {}) {
    this.#base = (options.base ?? "").replace(/\/$/, "");
    this.#fetch = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
    this.#WebSocket =
      options.WebSocket ??
      (globalThis.WebSocket as unknown as WebSocketLikeCtor | undefined);
  }

  /**
   * Calls `listener` when a request was refused with `401`.
   *
   * The app mounts the login screen on it. Returns the unsubscribe function, so
   * a React effect can clean up.
   */
  onUnauthorized(listener: () => void): () => void {
    this.#unauthorized.add(listener);
    return () => {
      this.#unauthorized.delete(listener);
    };
  }

  /**
   * Calls `listener` when the service answered `{code: "reauth"}`.
   *
   * That is a destructive verb that wants the password entered again; the app
   * shows its re-authenticate dialog on it.
   */
  onReauth(listener: () => void): () => void {
    this.#reauth.add(listener);
    return () => {
      this.#reauth.delete(listener);
    };
  }

  // --- the login door -----------------------------------------------------

  /**
   * `GET /api/session`: the live session, or `null` when there is none.
   *
   * A service older than the login routes answers `404`; that is a signed-out
   * app, not an error, so the login screen shows and the rest fails closed.
   */
  async session(): Promise<SessionInfo | null> {
    try {
      return await this.#get<SessionInfo>("/api/session");
    } catch (error) {
      if (error instanceof ApiError && (error.status === 401 || error.status === 404)) {
        return null;
      }
      throw error;
    }
  }

  /**
   * `POST /api/login`: trades the password for a session cookie.
   *
   * A wrong password is a `401` with `{code, message}` and a rate limit is a
   * `429` with `Retry-After`; neither signs the app out, because the app is
   * already signed out.
   */
  async login(password: string, remember: boolean): Promise<SessionInfo | null> {
    const body: LoginRequest = { password, remember };
    return this.#request<SessionInfo | null>("POST", "/api/login", {
      body,
      signOut: false,
    });
  }

  /** `POST /api/logout`: deletes the session and shows the login screen. */
  async logout(): Promise<void> {
    await this.#request<void>("POST", "/api/logout", { signOut: false });
    this.#emit(this.#unauthorized);
  }

  // --- reads --------------------------------------------------------------

  /** `GET /api/state`: the whole board, folded now. */
  state(): Promise<SnapshotDto> {
    return this.#get<SnapshotDto>("/api/state");
  }

  /**
   * `GET /api/changes?since=`: what changed after `since`.
   *
   * The service holds the request open for `waitMs` and answers `204` when
   * nothing happened, which is `null` here.
   */
  async changes(since: number, waitMs = DEFAULT_CHANGE_WAIT_MS): Promise<ChangeDto | null> {
    const seconds = Math.max(0, Math.ceil(waitMs / 1000));
    const response = await this.#send(
      "GET",
      `/api/changes?since=${since}&timeout=${seconds}`,
      undefined,
    );
    if (response.status === NO_CHANGE) {
      return null;
    }
    if (!response.ok) {
      throw await this.#failure(response);
    }
    return (await response.json()) as ChangeDto;
  }

  /** `GET /api/asks`: every ask still waiting for the person. */
  asks(): Promise<AskListDto> {
    return this.#get<AskListDto>("/api/asks");
  }

  /** `GET /api/graph`: where dagr is and the command that draws the run. */
  graph(): Promise<GraphDto> {
    return this.#get<GraphDto>("/api/graph");
  }

  /**
   * `GET /api/graph/run`: the run document dagr draws, verbatim.
   *
   * It is the same `<repo>/.dagr/run.json` the Herdr window's graph block hands
   * to `dagr view`, not one of the service's DTOs, so the answer is read with
   * `parseRunDocument` (`./rundoc`) rather than trusted to be one shape.
   */
  graphRun(): Promise<unknown> {
    return this.#get<unknown>("/api/graph/run");
  }

  /** `GET /api/settings`: every setting, its value and where it came from. */
  settings(): Promise<SettingsDto> {
    return this.#get<SettingsDto>("/api/settings");
  }

  /** `GET /api/roster`: the programs a role may run, with their models. */
  roster(): Promise<RosterDto> {
    return this.#get<RosterDto>("/api/roster");
  }

  /** `GET /api/prompts`: the role prompts this machine has. */
  prompts(): Promise<PromptListDto> {
    return this.#get<PromptListDto>("/api/prompts");
  }

  /** `GET /api/prompts/{id}`: one prompt's words. */
  prompt(id: string): Promise<PromptDto> {
    return this.#get<PromptDto>(`/api/prompts/${encodeURIComponent(id)}`);
  }

  /** `GET /api/tasks/{id}/changes`: the task's branch against its base. */
  taskChanges(id: string): Promise<TaskChangesDto> {
    return this.#get<TaskChangesDto>(`/api/tasks/${encodeURIComponent(id)}/changes`);
  }

  /** `GET /api/chat`: a space's conversation after `since`. */
  chat(space: string, since = ""): Promise<ChatDto> {
    const query = new URLSearchParams({ space });
    if (since !== "") {
      query.set("since", since);
    }
    return this.#get<ChatDto>(`/api/chat?${query.toString()}`);
  }

  // --- writes -------------------------------------------------------------

  /** `POST /api/action`: one action from the closed union. */
  action(action: Action): Promise<ActionReply> {
    return this.#write<ActionReply>("/api/action", action);
  }

  /** `POST /api/asks/answer`: answer one ask. */
  answerAsk(body: AnswerAsk): Promise<AskReply> {
    return this.#write<AskReply>("/api/asks/answer", body);
  }

  /** `POST /api/tasks`: add a task, as `shprd agent task add` does. */
  addTask(body: NewTaskDto): Promise<ActionReply> {
    return this.#write<ActionReply>("/api/tasks", body);
  }

  /** `POST /api/chat`: type one turn into a space's agent. */
  sendChat(body: ChatSendDto): Promise<ChatSendReply> {
    return this.#write<ChatSendReply>("/api/chat", body);
  }

  /** `PUT /api/settings`: write one settings key. */
  putSetting(body: SettingsPutDto): Promise<SettingsWriteDto> {
    return this.#request<SettingsWriteDto>("PUT", "/api/settings", { body });
  }

  /** `PUT /api/prompts/{id}`: store the person's words over a prompt. */
  putPrompt(id: string, body: PromptPutDto): Promise<PromptDto> {
    return this.#request<PromptDto>("PUT", `/api/prompts/${encodeURIComponent(id)}`, {
      body,
    });
  }

  /** `DELETE /api/prompts/{id}`: put the shipped words back. */
  resetPrompt(id: string): Promise<PromptDto> {
    return this.#request<PromptDto>("DELETE", `/api/prompts/${encodeURIComponent(id)}`);
  }

  // --- the socket ---------------------------------------------------------

  /**
   * `GET /api/ws`: the change notices, pushed.
   *
   * A message whose `type` this build does not know is ignored, never an error:
   * a phone that has not reloaded must keep working when the box updates
   * (M7d §4.5 rule 6). The session cookie travels with the handshake; the
   * service checks the upgrade's `Origin` as it does for every write.
   */
  connect(handlers: SocketHandlers): Socket {
    const Ctor = this.#WebSocket;
    if (!Ctor) {
      throw new ApiError(0, "unsupported", "this browser has no WebSocket");
    }
    const socket = new Ctor(this.#socketUrl());
    socket.onopen = () => handlers.open?.();
    socket.onclose = () => handlers.close?.();
    socket.onerror = () => handlers.close?.();
    socket.onmessage = (event) => {
      const message = parseMessage(event.data);
      if (!message) {
        return;
      }
      switch (message.type) {
        case "hello":
          handlers.hello?.(message);
          break;
        case "change":
          handlers.change?.(message.change);
          break;
        case "pong":
          break;
        default:
          // Forward compatibility: a newer service may send a kind this build
          // has never heard of. Ignoring it is the whole point of the union
          // being open on the wire.
          break;
      }
    };
    return {
      send: (message) => {
        socket.send(JSON.stringify(message));
      },
      close: () => socket.close(),
    };
  }

  // --- internals ----------------------------------------------------------

  #get<T>(path: string): Promise<T> {
    return this.#request<T>("GET", path);
  }

  #write<T>(path: string, body: unknown): Promise<T> {
    return this.#request<T>("POST", path, { body });
  }

  async #request<T>(
    method: string,
    path: string,
    options: { body?: unknown; signOut?: boolean } = {},
  ): Promise<T> {
    const response = await this.#send(method, path, options.body);
    if (!response.ok) {
      throw await this.#failure(response, options.signOut ?? true);
    }
    if (response.status === NO_CHANGE) {
      return undefined as T;
    }
    const text = await response.text();
    if (text === "") {
      return undefined as T;
    }
    return JSON.parse(text) as T;
  }

  async #send(method: string, path: string, body: unknown): Promise<Response> {
    const headers: Record<string, string> = { accept: "application/json" };
    if (body !== undefined) {
      headers["content-type"] = "application/json";
    }
    if (method !== "GET" && method !== "HEAD") {
      headers[WRITE_HEADER] = "1";
    }
    try {
      return await this.#fetch(`${this.#base}${path}`, {
        method,
        headers,
        credentials: "same-origin",
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
    } catch (cause) {
      throw new ApiError(
        0,
        "network",
        cause instanceof Error ? cause.message : "the service could not be reached",
      );
    }
  }

  async #failure(response: Response, signOut = true): Promise<ApiError> {
    const error = await readError(response);
    if (error.code === "reauth") {
      this.#emit(this.#reauth);
    } else if (signOut && error.signsOut) {
      this.#emit(this.#unauthorized);
    }
    return error;
  }

  #emit(listeners: Set<() => void>): void {
    for (const listener of listeners) {
      listener();
    }
  }

  #socketUrl(): string {
    if (this.#base !== "") {
      return `${this.#base.replace(/^http/, "ws")}/api/ws`;
    }
    const location = globalThis.location;
    const scheme = location?.protocol === "https:" ? "wss:" : "ws:";
    const host = location?.host ?? "127.0.0.1";
    return `${scheme}//${host}/api/ws`;
  }
}

/** Turns one socket frame into a message, or `null` when it is not one. */
function parseMessage(data: unknown): WsMessage | null {
  if (typeof data !== "string") {
    return null;
  }
  try {
    const message: unknown = JSON.parse(data);
    if (
      typeof message === "object" &&
      message !== null &&
      "type" in message &&
      typeof (message as { type: unknown }).type === "string"
    ) {
      return message as WsMessage;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Reads a refusal body, with the rate limit's `Retry-After`.
 *
 * The body is the service's `ErrorBody`; a proxy can answer HTML instead, and
 * then the status line is all there is to say.
 */
async function readError(response: Response): Promise<ApiError> {
  const retryAfter = retryAfterSeconds(response.headers.get("retry-after"));
  const body = (await response.json().catch(() => null)) as {
    code?: unknown;
    message?: unknown;
  } | null;
  const code =
    typeof body?.code === "string"
      ? body.code
      : response.status === 429
        ? "rate_limited"
        : "unsupported";
  const message =
    typeof body?.message === "string"
      ? body.message
      : `the service answered ${response.status}`;
  return new ApiError(
    response.status,
    code,
    message,
    retryAfter === undefined ? undefined : retryAfter,
  );
}

/** `Retry-After` as seconds; the header may also be an HTTP date, which we skip. */
function retryAfterSeconds(value: string | null): number | undefined {
  if (value === null) {
    return undefined;
  }
  const seconds = Number(value.trim());
  return Number.isFinite(seconds) && seconds >= 0 ? seconds : undefined;
}
