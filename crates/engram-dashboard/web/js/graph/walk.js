// The ] / [ walk over the drawn memories (views/graph.js `stepNode`).

/**
 * The position in `walk` (memory indices in walk order) that one step in
 * direction `dir` (1 or -1) from memory `cur` lands on. `cur` below 0 (no
 * memory focused: the graph box, or a memory not in the walk) starts from
 * the ends: `]` the first memory, `[` the last.
 */
export function stepIndex(walk, cur, dir) {
  const n = walk.length;
  if (!n) return -1;
  const at = cur >= 0 ? walk.indexOf(cur) : -1;
  if (at < 0) return dir > 0 ? 0 : n - 1;
  return (((at + dir) % n) + n) % n;
}
