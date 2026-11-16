// Registers the `herdr` stub before any test module loads.
//
//   node --import ./test/boot.mjs --test test/
//
// The stub is a module of its own (`./stub-herdr.js`), so a test drives the
// discovery and the watching without a Herdr window and without a network.

import { register } from "node:module";

register("./hooks.mjs", import.meta.url);
