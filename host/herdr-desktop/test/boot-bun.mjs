// Registers the `herdr` stub for bun, whose loader hook is `--preload` plus
// `mock.module` rather than node's `register`.
//
//   bun test test/
//
// A Herdr mod imports `herdr` and nothing else of the app's; this points that
// one specifier at `./stub-herdr.js` and leaves every other import alone.

import { mock } from "bun:test";

import * as stub from "./stub-herdr.js";

mock.module("herdr", () => stub);
