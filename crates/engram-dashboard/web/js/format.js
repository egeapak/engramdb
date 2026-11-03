// Number and date formatting.

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const MIN = 60 * 1000;
const DAY = 24 * 60 * MIN;

export function fmt(n) {
  return Number(n || 0).toLocaleString('en-US');
}

/** "5 min ago", "3 days ago", "never". */
export function ago(iso, now = Date.now()) {
  if (!iso) return 'never';
  const t = Date.parse(iso);
  if (!isFinite(t)) return 'unknown';
  const m = (now - t) / MIN;
  if (m < 0) {
    const d = Math.round(-m / (60 * 24));
    return d <= 0 ? 'later today' : `in ${d} day${d === 1 ? '' : 's'}`;
  }
  if (m < 1) return 'just now';
  if (m < 60) return `${Math.round(m)} min ago`;
  if (m < 60 * 24) return `${Math.round(m / 60)} h ago`;
  const d = Math.round(m / (60 * 24));
  return `${d} day${d === 1 ? '' : 's'} ago`;
}

export function daysSince(iso, now = Date.now()) {
  if (!iso) return Infinity;
  return (now - Date.parse(iso)) / DAY;
}

/** "2 Nov 2026, 14:05" in local time. */
export function stamp(iso) {
  if (!iso) return '—';
  const d = new Date(iso);
  if (!isFinite(d.getTime())) return String(iso);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}, ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** "2 Nov" for a YYYY-MM-DD day. */
export function dayLabel(day) {
  const [y, m, d] = String(day).split('-').map(Number);
  if (!y) return String(day);
  return `${d} ${MONTHS[m - 1]}`;
}

export function niceMax(v) {
  if (v <= 0) return 1;
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}

/**
 * A short label for an id. UUIDv7 ids made in the same millisecond share
 * their first 12 hex digits, so a bare prefix would read as identical; the
 * random tail tells them apart.
 */
export function shortId(id) {
  const s = String(id || '');
  return s.length > 16 ? `${s.slice(0, 8)}…${s.slice(-4)}` : s;
}

export function plural(n, one, many) {
  return `${fmt(n)} ${n === 1 ? one : many || one + 's'}`;
}

/** "needs_review" → "Needs review". */
export function label(word) {
  const s = String(word || '').replace(/_/g, ' ');
  return s.charAt(0).toUpperCase() + s.slice(1);
}
