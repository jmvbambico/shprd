// The one Node API a test reads a file with (the app's tsconfig has no Node types).
declare module "node:fs" {
  /** Reads a file as text. */
  export function readFileSync(path: URL, encoding: "utf8"): string;
}
