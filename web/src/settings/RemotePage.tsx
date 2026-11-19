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
import { Dropdown } from "./Dropdown";
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
      <p className="snote chain-note">
        Anyone with the link can reach shprd, so the link stays hidden until you choose to show it.
      </p>
    </div>
  );
}

/** An error in the Agents page's style: a red box with the service's own sentence. */
function Refusal({ text }: { text: string }) {
  return (
    <div className="refusal" role="alert">
      <div className="refusal-text">{text}</div>
    </div>
  );
}

/** A section heading with its first lines. */
function Heading({ id, title }: { id: string; title: string }) {
  return (
    <h3 id={id} className="rsection-title">
      {title}
    </h3>
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
      <Heading id="remote-tunnel" title="Tunnel" />
      {load.state === "loading" ? <p className="snote">Reading the tunnel…</p> : null}
      {load.state === "unavailable" ? (
        <p className="snote">Tunnels are not available on this service.</p>
      ) : null}
      {load.state === "error" ? <Refusal text={load.message} /> : null}
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
  const showLink = status.on && Boolean(status.link);
  return (
    <div className="tunnel" data-tunnel={status.on ? "on" : "off"}>
      <div className="srow" data-row="tunnel">
        <div className="srow-main">
          <div className="srow-words">
            <div className="srow-title">
              <span className={status.on ? "dot tone-green" : "dot"} aria-hidden="true" />
              Tunnel
            </div>
            <div className="srow-about" data-status={status.on ? "on" : starting ? "starting" : "off"}>
              {status.on ? (
                showLink ? (
                  <>
                    On at{" "}
                    <code className="link-text" data-revealed={revealed}>
                      {revealed ? status.link : maskLink(status.link ?? "")}
                    </code>{" "}
                    <button type="button" className="link" aria-pressed={revealed} onClick={onReveal}>
                      {revealed ? "Hide" : "Reveal"}
                    </button>
                  </>
                ) : (
                  "On"
                )
              ) : starting ? (
                "Starting…"
              ) : (
                "Off"
              )}
            </div>
          </div>
          <div className="srow-control">
            {status.on ? (
              <button type="button" className="btn" disabled={busy !== null} onClick={onStop}>
                {busy === "stop" ? "Stopping…" : "Stop tunnel"}
              </button>
            ) : (
              <button type="button" className="btn" disabled={starting} onClick={onStart}>
                {starting ? "Starting…" : "Start tunnel"}
              </button>
            )}
          </div>
        </div>
        {status.on && status.qr_svg && revealed ? (
          <img
            className="qr"
            alt="QR code for the link"
            src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(status.qr_svg)}`}
          />
        ) : null}
      </div>
      {status.on && status.provider ? (
        <p className="snote">
          Carried by {status.provider}
          {status.version ? ` ${status.version}` : ""}.
        </p>
      ) : null}
      {!status.on && status.why ? <p className="snote">{status.why}</p> : null}
      {status.tailnet_user ? (
        <p className="snote">Signed in through Tailscale as {status.tailnet_user}.</p>
      ) : null}

      {!status.on ? (
        <div className="srow" data-row="provider">
          <div className="srow-main">
            <div className="srow-words">
              <div className="srow-title">Provider</div>
              <div className="srow-about">What carries the tunnel.</div>
            </div>
            <div className="srow-control">
              <Dropdown
                label="Provider"
                value={provider}
                disabled={starting}
                options={status.providers.map((entry) => ({
                  id: entry.id,
                  label: entry.public_internet ? `${entry.id} (public internet)` : entry.id,
                }))}
                onPick={onProvider}
              />
            </div>
          </div>
        </div>
      ) : null}
      {!status.on && needsPassword ? (
        <p className="snote">
          A public tunnel needs a login password first: run `shprd web password set`.
        </p>
      ) : null}
      {refusal !== null ? <Refusal text={refusal} /> : null}
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
      <Heading id="remote-sessions" title="Signed-in devices" />
      {load.state === "loading" ? <p className="snote">Reading the devices…</p> : null}
      {load.state === "unavailable" ? (
        <p className="snote">The list of signed-in devices is not available on this service.</p>
      ) : null}
      {load.state === "error" ? <Refusal text={load.message} /> : null}
      {load.state === "data" && load.data.sessions.length === 0 ? (
        <p className="snote">
          No devices yet. A phone or computer shows up here once it has logged in.
        </p>
      ) : null}
      {load.state === "data" && load.data.sessions.length > 0
        ? load.data.sessions.map((session) => {
            const here = session.id === load.data.current;
            return (
              <div key={session.id} className="srow" data-session={session.id}>
                <div className="srow-main">
                  <div className="srow-words">
                    <div className="srow-title">
                      {session.label}
                      {here ? <span className="badge">this device</span> : null}
                      {session.remember ? <span className="badge">remembered</span> : null}
                    </div>
                    <div className="srow-about">Last seen {ago(session.last_seen)}</div>
                  </div>
                  <div className="srow-control">
                    <button
                      type="button"
                      className="btn"
                      aria-label={`Revoke ${session.label} (${session.id})`}
                      disabled={revoking !== null}
                      onClick={() => void revoke(session.id, here)}
                    >
                      {revoking === session.id ? "Signing out…" : here ? "Sign out" : "Sign out device"}
                    </button>
                  </div>
                </div>
              </div>
            );
          })
        : null}
      {refusal !== null ? <Refusal text={refusal} /> : null}
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
      <Heading id="remote-logout" title="This device" />
      <div className="srow" data-row="logout">
        <div className="srow-main">
          <div className="srow-words">
            <div className="srow-title">Log out</div>
            <div className="srow-about">Sign out of shprd on this device.</div>
          </div>
          <div className="srow-control">
            <button type="button" className="btn" disabled={busy} onClick={() => void logout()}>
              Log out
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}
