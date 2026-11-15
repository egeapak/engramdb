// What the page derives for the server's layouts: the memories' label text
// and the box's rounded shape (the server lays out for it: `layout_aspect=`
// / `layout_width=`). The server builds each layout's input from the
// response it sends (src/layout/ in the dashboard crate).

/** The widest box the server lays out (`layout::MAX_LAYOUT_WIDTH`). */
export const MAX_LAYOUT_WIDTH = 20000;

/**
 * A memory's short label: the summary up to its first clause break, cut on
 * a word boundary. (Titles are not in the index, so the summary is what a
 * graph read can show.) The server measures labels as this text
 * (`layout::short_label_len`), so a label's box fits what is drawn.
 */
export function shortLabel(summary, max = 24) {
  let t = String(summary || '').trim();
  const cut = t.search(/[:;—–(]|\.\s|,\s| - /);
  if (cut > 8) t = t.slice(0, cut).trim();
  if (t.length <= max) return t;
  const words = t.slice(0, max).split(/\s+/);
  if (words.length > 1) words.pop();
  return words.join(' ').replace(/[,.:;\s]+$/, '') + '…';
}

/** The width laid out for a box that was not measured (hidden, or not laid out yet). */
export const DEFAULT_LAYOUT_WIDTH = 800;

/**
 * The box's aspect as the server takes it (a multiple of 0.05, within
 * 0.3–3); a non-finite one (an unmeasured box) is 0.6, never `NaN`, which
 * the server refuses.
 */
export const roundAspect = (a) => (Number.isFinite(a) ? Math.round(Math.min(3, Math.max(0.3, a)) * 20) / 20 : 0.6);

/**
 * The box's width as the server takes it: a multiple of 40 px from 40 to
 * MAX_LAYOUT_WIDTH. A width that is not finite or under 20 px is a box
 * that was not measured, laid out at DEFAULT_LAYOUT_WIDTH: clamping it to
 * 40 would size the scope names for a tiny box, and they keep that size
 * when the box widens (a resize only re-fits).
 */
export const roundWidth = (w) => (Number.isFinite(w) && w >= 20 ? Math.round(Math.min(MAX_LAYOUT_WIDTH, Math.max(40, w)) / 40) * 40 : DEFAULT_LAYOUT_WIDTH);
