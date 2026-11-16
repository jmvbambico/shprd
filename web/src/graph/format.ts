// Small formatters the graph screens share, written once so a node, a header
// and a detail cannot say the same number in two ways.

/** `n` things, with a singular and plural word: `1 task`, `3 dependencies`. */
export function count(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/**
 * A document time (RFC 3339 UTC) as a person reads it, in their own time zone.
 *
 * A value that is not a time is handed back untouched: the document's times come
 * from another program, and "when" showing the raw text is better than hiding
 * the field.
 */
export function when(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) {
    return iso;
  }
  return at.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}
