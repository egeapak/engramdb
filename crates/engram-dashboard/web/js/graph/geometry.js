// Polyline helpers shared by the core graph view (tooltip and pin
// positions) and the styles that draw relations along the layout's paths.

/** An SVG path through `pts` ([[x, y], …]). */
export function pathD(pts) {
  return 'M' + pts.map((p) => `${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(' L');
}

/** The point at fraction `t` of the polyline's length, with the heading there. */
export function along(pts, t) {
  let total = 0;
  for (let i = 1; i < pts.length; i++) total += Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]);
  let want = total * t;
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1];
    const b = pts[i];
    const l = Math.hypot(b[0] - a[0], b[1] - a[1]);
    if (want <= l || i === pts.length - 1) {
      const k = l ? Math.min(1, want / l) : 0;
      return { x: a[0] + (b[0] - a[0]) * k, y: a[1] + (b[1] - a[1]) * k, ang: Math.atan2(b[1] - a[1], b[0] - a[0]) };
    }
    want -= l;
  }
  return { x: pts[0][0], y: pts[0][1], ang: 0 };
}

/** The point `d` along the first (or, with `fromEnd`, back along the last) segment. */
export function offset(pts, d, fromEnd) {
  const [a, b] = fromEnd ? [pts[pts.length - 1], pts[pts.length - 2]] : [pts[0], pts[1]];
  const l = Math.hypot(b[0] - a[0], b[1] - a[1]) || 1;
  const k = Math.min(d, l / 2) / l;
  const ang = Math.atan2(a[1] - b[1], a[0] - b[0]);
  return { x: a[0] + (b[0] - a[0]) * k, y: a[1] + (b[1] - a[1]) * k, ang: fromEnd ? ang : ang + Math.PI };
}
