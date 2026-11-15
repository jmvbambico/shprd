// Resolves `herdr` to the stub, and nothing else: a Herdr mod imports the app's
// own modules by name, and the unit test has no app.
//
// It is node's loader hook (`node --import ./test/boot.mjs --test test/`); bun
// runs the same tests with its own `--preload` (see `test/boot.mjs`).

import { pathToFileURL } from "node:url";

const STUB = pathToFileURL(new URL("./stub-herdr.js", import.meta.url).pathname).href;

export async function resolve(specifier, context, next) {
  if (specifier === "herdr") return { url: STUB, shortCircuit: true };
  return next(specifier, context);
}
