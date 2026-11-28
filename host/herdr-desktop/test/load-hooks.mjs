// Loader hook for `load-mods.mjs`: answers `herdr`, `gpui-kit`, `gpui-base` and
// `gpui` with a module that exports every name the mods import (a callable
// stand-in), so loading a mod checks its own source and not the stubs' coverage.
// The names come from `LOAD_MODS_NAMES` (JSON: specifier to a list of names).

const names = JSON.parse(process.env.LOAD_MODS_NAMES ?? "{}");

export async function resolve(specifier, context, next) {
  if (specifier in names) return { url: `shprd-stub:${specifier}`, shortCircuit: true };
  return next(specifier, context);
}

export async function load(url, context, next) {
  if (!url.startsWith("shprd-stub:")) return next(url, context);
  const list = names[url.slice("shprd-stub:".length)] ?? [];
  const src = [
    "const make = () => new Proxy(function () {}, { get: (_, k) => k === 'then' ? undefined : make(), apply: () => make(), construct: () => make() });",
    ...list.map((n) => `export const ${n} = make();`),
  ].join("\n");
  return { format: "module", source: src, shortCircuit: true };
}
