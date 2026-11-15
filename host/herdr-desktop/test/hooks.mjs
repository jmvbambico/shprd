// Resolves the app's own modules to stubs, and nothing else: a Herdr mod imports
// `herdr`, `gpui-kit` and `gpui-base` by name, and the unit test has no app.
//
// It is node's loader hook (`node --import ./test/boot.mjs --test test/`); bun
// runs the same tests with its own `--preload` (see `test/boot-bun.mjs`).

import { pathToFileURL } from "node:url";

const herdr = pathToFileURL(new URL("./stub-herdr.js", import.meta.url).pathname).href;
const gpui = pathToFileURL(new URL("./stub-gpui.js", import.meta.url).pathname).href;

/** The app's modules, and the stub each one is answered by. */
const STUBS = {
  herdr,
  "gpui-kit": gpui,
  "gpui-base": gpui,
  gpui: gpui,
};

export async function resolve(specifier, context, next) {
  const stub = STUBS[specifier];
  if (stub) return { url: stub, shortCircuit: true };
  return next(specifier, context);
}
