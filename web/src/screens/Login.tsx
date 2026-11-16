// The login screen, and the dialog that asks for the password again.
//
// It posts the password to `/api/login` (M11.1's route) and nothing else; the
// service sets the `__Host-` cookie, so the app never holds a credential.

import { useId, useState, type FormEvent } from "react";

import { ApiError, type ApiClient } from "../api/client";

/** What the login card is for. */
export interface LoginProps {
  /** The client that posts the password. */
  client: ApiClient;
  /** Called once the service accepted the password. */
  onSignedIn: () => void;
  /** True when this is the "enter it again" dialog, not the first sign-in. */
  reauth?: boolean;
}

/**
 * One line a person can act on for a failed sign-in.
 *
 * A wrong password is the service's own line (which never says whether the
 * password exists), a rate limit says how long to wait, and an unreachable
 * service says so rather than blaming the password.
 */
export function loginMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 429) {
      const wait = error.retryAfter;
      return wait === undefined
        ? "Too many attempts. Wait a while and try again."
        : `Too many attempts. Try again in ${Math.ceil(wait)} s.`;
    }
    if (error.status === 0) {
      return "Can't reach shprd. Is the service running?";
    }
    return error.message;
  }
  return error instanceof Error ? error.message : "Something went wrong.";
}

/** The password form. */
export function Login({ client, onSignedIn, reauth = false }: LoginProps) {
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const passwordId = useId();
  const rememberId = useId();

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || password === "") {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await client.login(password, remember);
      setPassword("");
      onSignedIn();
    } catch (failure) {
      setError(loginMessage(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="login" onSubmit={submit} data-reauth={reauth}>
      <div className="login-brand" aria-hidden="true">
        <span className="brand-mark" />
        <span className="brand-word">shprd</span>
      </div>
      <h1 className="login-title">{reauth ? "Enter your password again" : "Sign in"}</h1>
      <p className="login-note">
        {reauth
          ? "That action needs a fresh password. Nothing was done."
          : "This machine's shprd service. One password, the one you set with `shprd web password set`."}
      </p>

      <label className="field" htmlFor={passwordId}>
        <span className="field-label">Password</span>
        <input
          id={passwordId}
          className="input"
          type="password"
          name="password"
          autoComplete="current-password"
          autoFocus
          required
          value={password}
          onChange={(event) => setPassword(event.target.value)}
          disabled={busy}
        />
      </label>

      <label className="check" htmlFor={rememberId}>
        <input
          id={rememberId}
          type="checkbox"
          checked={remember}
          onChange={(event) => setRemember(event.target.checked)}
          disabled={busy}
        />
        <span>Remember this device</span>
      </label>

      {error === null ? null : (
        <p className="login-error" role="alert">
          {error}
        </p>
      )}

      <button className="button login-submit" type="submit" disabled={busy || password === ""}>
        {busy ? "Signing in…" : "Sign in"}
      </button>
    </form>
  );
}
