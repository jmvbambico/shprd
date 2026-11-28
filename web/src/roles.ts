// What a role is called on screen: the person's own name for it, else the
// built-in word. `role_titles` on `GET /api/state` carries the service's answer
// (trimmed, capped and with the fallback already applied); these are only the
// words used when a state carries none.

/** The words the shipped roles are called by until the person renames them. */
export const ROLE_DEFAULTS: Readonly<Record<string, string>> = {
  coordinator: "Assistant",
  worker: "Worker",
  reviewer: "Reviewer",
};

/** The longest name a role may have: the service's own cap. */
export const TITLE_MAX = 24;

/** What `role` is called: its name, else its built-in word, else its key. */
export function roleTitle(titles: Readonly<Record<string, string | undefined>> | undefined | null, role: string): string {
  const named = titles?.[role]?.trim();
  return named || ROLE_DEFAULTS[role] || role;
}

/** A typed name as it is written: trimmed, runs of space as one, cut to the cap. */
export function cleanTitle(text: string): string {
  return text.split(/\s+/).filter(Boolean).join(" ").slice(0, TITLE_MAX).trimEnd();
}

/** Whether a change to this key reaches a running coordinator only once it restarts. */
export function needsCoordinatorRestart(key: string): boolean {
  return /^roles\.coordinator\.(agent|model|effort|fast|perm)$/.test(key);
}
