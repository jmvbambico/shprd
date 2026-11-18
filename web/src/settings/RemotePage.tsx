// Remote: the tunnel, the link behind it, the devices signed in, and logging out.
//
// Everything here is the service's word. The tunnel is `GET /api/tunnels` (and
// its start and stop); the devices are `GET /api/sessions`. A service too old to
// have a route answers `404`, and the section says it is not available rather
// than pretending to be empty. Nothing here logs or stores a link: it lives in
// component state for as long as the page is open, and is masked until the
// person asks to see it.

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, type ApiClient } from "../api/client";
import type { SessionListDto, TunnelStatusDto } from "../gen";
import { reason } from "./useSettings";

/** How a section's read stands. */
type Load<T> =
  | { state: "loading" }
  | { state: "unavailable" }
  | { state: "error"; message: string }
  | { state: "data"; data: T };

/** Reads one route, mapping a `404` to "not available". */
function useLoad<T>(read: () => Promise<T>): [Load<T>, (data: T) => void, () => void] {
  const [load, setLoad] = useState<Load<T>>({ state: "loading" });
  const alive = useRef(true);
  const readRef = useRef(read);
  readRef.current = read;

  const reload = useCallback(() => {
    readRef
      .current()
      .then((data) => {
        if (alive.current) {
          setLoad({ state: "data", data });
        }
      })
      .catch((error: unknown) => {
        if (!alive.current) {
          return;
        }
        if (error instanceof ApiError && error.status === 404) {
          setLoad({ state: "unavailable" });
        } else {
          setLoad({ state: "error", message: reason(error) });
        }
      });
  }, []);

  useEffect(() => {
    alive.current = true;
    reload();
    return () => {
      alive.current = false;
    };
  }, [reload]);

  return [load, (data) => setLoad({ state: "data", data }), reload];
}

/** The link with its secret part hidden: the scheme and the last label remain. */
export function maskLink(link: string): string {
  const match = /^(https?:\/\/)([^/]+)(.*)$/.exec(link);
  if (!match) {
    return "••••••••";
  }
  const host = match[2] ?? "";
  const dot = host.indexOf(".");
  const rest = dot > 0 ? host.slice(dot) : "";
  return `${match[1] ?? ""}••••••${rest}/••••`;
}

/** "5 min ago" for a time in epoch seconds. */
export function ago(seconds: number, now = Date.now() / 1000): string {
  const gap = Math.max(0, Math.round(now - seconds));
  if (gap < 60) {
    return "just now";
  }
  if (gap < 3600) {
    return `${Math.floor(gap / 60)} min ago`;
  }
  if (gap < 86_400) {
    return `${Math.floor(gap / 3600)} h ago`;
  }
  return `${Math.floor(gap / 86_400)} d ago`;
}

/** The Remote page. */
export function RemotePage({
  client,
  onSignedOut,
}: {
  client: ApiClient;
  onSignedOut: () => void;
}) {
  return (
    <div className="spage" data-page="remote">
      <h2 className="spage-title">Remote</h2>
      <TunnelSection client={client} />
      <SessionsSection client={client} onSignedOut={onSignedOut} />
      <LogoutSection client={client} onSignedOut={onSignedOut} />
    </div>
  );
}

function TunnelSection({ client }: { client: ApiClient }) {
  const [load, set] = useLoad(() => client.tunnels());
  const [provider, setProvider] = useState<string | null>(null);
  const [busy, setBusy] = useState<"start" | "stop" | null>(null);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [revealed, setRevealed] = useState(false);

  async function act(kind: "start" | "stop", chosen: string) {
    setBusy(kind);
    setRefusal(null);
    try {
      const next =
        kind === "start" ? await client.startTunnel({ provider: chosen }) : await client.stopTunnel();
      set(next);
      setRevealed(false);
    } catch (error) {
      // The service's own sentence: a public tunnel with no password is refused in
      // words that say so, and the person needs those words.
      setRefusal(reason(error));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="rsection" aria-labelledby="remote-tunnel">
      <h3 id="remote-tunnel" className="rsection-title">
        Tunnel
      </h3>
      {load.state === "loading" ? <p className="snote">Reading the tunnel…</p> : null}
      {load.state === "unavailable" ? (
        <p className="snote">Tunnels are not available on this service.</p>
      ) : null}
      {load.state === "error" ? (
        <p className="refusal-text" role="alert">
          {load.message}
        </p>
      ) : null}
      {load.state === "data" ? (
        <TunnelBody
          status={load.data}
          provider={provider ?? load.data.default_provider}
          onProvider={setProvider}
          busy={busy}
          refusal={refusal}
          revealed={revealed}
          onReveal={() => setRevealed((value) => !value)}
          onStart={() => void act("start", provider ?? load.data.default_provider)}
          onStop={() => void act("stop", "")}
        />
      ) : null}
    </section>
  );
}

function TunnelBody({
  status,
  provider,
  onProvider,
  busy,
  refusal,
  revealed,
  onReveal,
  onStart,
  onStop,
}: {
  status: TunnelStatusDto;
  provider: string;
  onProvider: (id: string) => void;
  busy: "start" | "stop" | null;
  refusal: string | null;
  revealed: boolean;
  onReveal: () => void;
  onStart: () => void;
  onStop: () => void;
}) {
  const starting = status.starting || busy === "start";
  const chosen = status.providers.find((entry) => entry.id === provider);
  const needsPassword = chosen?.public_internet === true && !status.password_set;
  const word = status.on ? "On" : starting ? "Starting…" : "Off";
  return (
    <div className="tunnel" data-tunnel={status.on ? "on" : "off"}>
      <div className="tunnel-state">
        <span className={status.on ? "dot tone-green" : "dot"} aria-hidden="true" />
        <span className="tunnel-word">{word}</span>
        {status.on && status.provider ? <span className="tunnel-provider">{status.provider}</span> : null}
        {status.version ? <span className="tunnel-version">{status.version}</span> : null}
      </div>
      {!status.on && status.why ? <p className="snote">{status.why}</p> : null}
      {status.tailnet_user ? (
        <p className="snote">Signed in through Tailscale as {status.tailnet_user}.</p>
      ) : null}

      {status.on && status.link ? (
        <div className="tunnel-link">
          <div className="link-line">
            <code className="link-text" data-revealed={revealed}>
              {revealed ? status.link : maskLink(status.link)}
            </code>
            <button type="button" className="link" aria-pressed={revealed} onClick={onReveal}>
              {revealed ? "Hide" : "Reveal"}
            </button>
          </div>
          {status.qr_svg && revealed ? (
            <img
              className="qr"
              alt="QR code for the link"
              src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(status.qr_svg)}`}
            />
          ) : null}
        </div>
      ) : null}

      {status.on ? (
        <button type="button" className="button" disabled={busy !== null} onClick={onStop}>
          {busy === "stop" ? "Stopping…" : "Stop tunnel"}
        </button>
      ) : (
        <div className="tunnel-start">
          <label className="field">
            <span className="field-label">Provider</span>
            <select
              className="input"
              value={provider}
              disabled={starting}
              onChange={(event) => onProvider(event.target.value)}
            >
              {status.providers.map((entry) => (
                <option key={entry.id} value={entry.id}>
                  {entry.id}
                  {entry.public_internet ? " (public internet)" : ""}
                </option>
              ))}
            </select>
          </label>
          {needsPassword ? (
            <p className="snote">
              A public tunnel needs a login password first: run `shprd web password set`.
            </p>
          ) : null}
          <button type="button" className="button" disabled={starting} onClick={onStart}>
            {starting ? "Starting…" : "Start tunnel"}
          </button>
        </div>
      )}
      {refusal !== null ? (
        <div className="refusal" role="alert">
          <div className="refusal-key">tunnel</div>
          <div className="refusal-text">{refusal}</div>
        </div>
      ) : null}
    </div>
  );
}

function SessionsSection({
  client,
  onSignedOut,
}: {
  client: ApiClient;
  onSignedOut: () => void;
}) {
  const [load, , reload] = useLoad<SessionListDto>(() => client.sessions());
  const [refusal, setRefusal] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  async function revoke(id: string, here: boolean) {
    setRevoking(id);
    setRefusal(null);
    try {
      await client.revokeSession(id);
      if (here) {
        // This device's own session is gone: the app shows the login screen now,
        // not after the next request happens to fail.
        onSignedOut();
        return;
      }
      reload();
    } catch (error) {
      setRefusal(reason(error));
    } finally {
      setRevoking(null);
    }
  }

  return (
    <section className="rsection" aria-labelledby="remote-sessions">
      <h3 id="remote-sessions" className="rsection-title">
        Signed in
      </h3>
      {load.state === "loading" ? <p className="snote">Reading the sessions…</p> : null}
      {load.state === "unavailable" ? (
        <p className="snote">The list of signed-in devices is not available on this service.</p>
      ) : null}
      {load.state === "error" ? (
        <p className="refusal-text" role="alert">
          {load.message}
        </p>
      ) : null}
      {load.state === "data" && load.data.sessions.length === 0 ? (
        <p className="snote">No device is signed in.</p>
      ) : null}
      {load.state === "data" && load.data.sessions.length > 0 ? (
        <ul className="session-list">
          {load.data.sessions.map((session) => {
            const here = session.id === load.data.current;
            return (
              <li key={session.id} className="session" data-session={session.id}>
                <div className="session-main">
                  <span className="session-label">{session.label}</span>
                  {here ? <span className="badge">this device</span> : null}
                  {session.remember ? <span className="session-kind">remembered</span> : null}
                </div>
                <div className="session-sub">last seen {ago(session.last_seen)}</div>
                <button
                  type="button"
                  className="link"
                  aria-label={`Revoke ${session.label} (${session.id})`}
                  disabled={revoking !== null}
                  onClick={() => void revoke(session.id, here)}
                >
                  {revoking === session.id ? "Revoking…" : here ? "Sign out" : "Revoke"}
                </button>
              </li>
            );
          })}
        </ul>
      ) : null}
      {refusal !== null ? (
        <div className="refusal" role="alert">
          <div className="refusal-key">sessions</div>
          <div className="refusal-text">{refusal}</div>
        </div>
      ) : null}
    </section>
  );
}

function LogoutSection({
  client,
  onSignedOut,
}: {
  client: ApiClient;
  onSignedOut: () => void;
}) {
  const [busy, setBusy] = useState(false);
  async function logout() {
    setBusy(true);
    try {
      await client.logout();
    } catch {
      // A failed logout still shows the login screen; the session ends by expiry
      // or by `shprd web sessions --revoke`.
    }
    onSignedOut();
  }
  return (
    <section className="rsection" aria-labelledby="remote-logout">
      <h3 id="remote-logout" className="rsection-title">
        This device
      </h3>
      <button type="button" className="button" disabled={busy} onClick={() => void logout()}>
        Log out
      </button>
    </section>
  );
}
