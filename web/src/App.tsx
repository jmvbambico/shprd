// The app: ask the service who we are, then show the login screen or the shell.
//
// This is also where the two client signals land: a `401` from any route puts
// the login screen back, and `{code: "reauth"}` opens the dialog that asks for
// the password again.

import { useEffect, useMemo, useState } from "react";

import { ApiClient } from "./api/client";
import { Login } from "./screens/Login";
import { Shell } from "./screens/Shell";

/** What the page is doing. */
type Phase = "checking" | "signed-in" | "signed-out";

/** The root component. */
export function App({ client }: { client?: ApiClient }) {
  const api = useMemo(() => client ?? new ApiClient(), [client]);
  const [phase, setPhase] = useState<Phase>("checking");
  const [reauth, setReauth] = useState(false);

  // `GET /api/session` is the first request: no session means the login screen
  // and nothing behind it. A service too old to have the route reads as signed
  // out too, which is the fail-closed direction.
  useEffect(() => {
    let live = true;
    api
      .session()
      .then((session) => {
        if (live) {
          setPhase(session === null ? "signed-out" : "signed-in");
        }
      })
      .catch(() => {
        if (live) {
          setPhase("signed-out");
        }
      });
    return () => {
      live = false;
    };
  }, [api]);

  useEffect(
    () =>
      api.onUnauthorized(() => {
        setReauth(false);
        setPhase("signed-out");
      }),
    [api],
  );

  useEffect(() => api.onReauth(() => setReauth(true)), [api]);

  if (phase === "checking") {
    return (
      <main className="boot" aria-busy="true">
        <span className="brand-mark" aria-hidden="true" />
        <p className="boot-word">Connecting to shprd…</p>
      </main>
    );
  }

  if (phase === "signed-out") {
    return (
      <main className="gate">
        <Login client={api} onSignedIn={() => setPhase("signed-in")} />
      </main>
    );
  }

  return (
    <Shell
      client={api}
      reauth={reauth}
      onReauthClose={() => setReauth(false)}
      onSignedOut={() => setPhase("signed-out")}
    />
  );
}
