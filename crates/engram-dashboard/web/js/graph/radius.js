// A memory's drawn size: shared by the core view (which draws every node at
// it) and any style that draws around nodes (Plain's regions), so both
// agree without a style importing the view.

/** Reads of a memory: every read signal, for size only (never shown summed). */
export const reads = (n) => n.usage.returned + n.usage.injected + n.usage.fetched;

/** A node's radius in layout units: 5, growing with reads up to 11. */
export const radius = (n) => 5 + Math.min(6, Math.sqrt(reads(n)) * 0.55);
