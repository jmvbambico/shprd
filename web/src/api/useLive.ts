// React's view of the live feed.

import { useEffect, useRef } from "react";

import type { ApiClient } from "./client";
import { LiveFeed, type LiveOptions } from "./live";

/**
 * Calls `refresh` whenever the service says the run changed.
 *
 * The callback is kept in a ref, so a screen that rebuilds its loader every
 * render does not restart the socket every render; the feed lives as long as the
 * `client` does.
 */
export function useLiveRefresh(
  client: Pick<ApiClient, "connect" | "changes">,
  refresh: () => void,
  options: LiveOptions = {},
): void {
  const latest = useRef(refresh);
  latest.current = refresh;

  useEffect(() => {
    const feed = new LiveFeed(client, () => latest.current(), options);
    feed.start();
    return () => feed.stop();
    // The options are a literal at every call site, so they are read once: a
    // feed restarted on every render would be a socket per render.
  }, [client]);
}
