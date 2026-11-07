// Layout quality metrics, all measured after fitting the layout into a
// W x H viewport (what the user sees on "Fit"), so layouts with different
// internal units compare fairly.

function segX(a, b, c, d) {
  // proper intersection of segments ab and cd (shared endpoints do not count)
  const o = (p, q, r) => Math.sign((q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]));
  return o(a, b, c) * o(a, b, d) < 0 && o(c, d, a) * o(c, d, b) < 0;
}

export function crossings(segs) {
  // segs: [[[x,y],[x,y], ownerA, ownerB]]; grid-bucketed by bbox
  let count = 0;
  const cell = 60;
  const grid = new Map();
  segs.forEach((s, k) => {
    const x0 = Math.floor(Math.min(s[0][0], s[1][0]) / cell), x1 = Math.floor(Math.max(s[0][0], s[1][0]) / cell);
    const y0 = Math.floor(Math.min(s[0][1], s[1][1]) / cell), y1 = Math.floor(Math.max(s[0][1], s[1][1]) / cell);
    const seen = new Set();
    for (let gx = x0; gx <= x1; gx++) for (let gy = y0; gy <= y1; gy++) {
      const key = gx + ',' + gy;
      const b = grid.get(key);
      if (b) for (const j of b) {
        if (seen.has(j)) continue;
        seen.add(j);
        const t = segs[j];
        if (s[2] === t[2] || s[2] === t[3] || s[3] === t[2] || s[3] === t[3]) continue;
        if (segX(s[0], s[1], t[0], t[1])) count++;
      }
    }
    for (let gx = x0; gx <= x1; gx++) for (let gy = y0; gy <= y1; gy++) {
      const key = gx + ',' + gy;
      if (!grid.has(key)) grid.set(key, []);
      grid.get(key).push(k);
    }
  });
  return count;
}

export function measure(g, res, W = 1240, H = 780) {
  const { nodes, edges } = g;
  const n = nodes.length;
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (let i = 0; i < n; i++) {
    x0 = Math.min(x0, res.x[i]); x1 = Math.max(x1, res.x[i]);
    y0 = Math.min(y0, res.y[i]); y1 = Math.max(y1, res.y[i]);
  }
  const s = Math.min(W / Math.max(1, x1 - x0), H / Math.max(1, y1 - y0));
  const X = (i) => (res.x[i] - x0) * s;
  const Y = (i) => (res.y[i] - y0) * s;
  const P = (i) => [X(i), Y(i)];
  // nearest-neighbour distances (grid)
  const cell = 20;
  const grid = new Map();
  for (let i = 0; i < n; i++) {
    const k = Math.floor(X(i) / cell) + ',' + Math.floor(Y(i) / cell);
    if (!grid.has(k)) grid.set(k, []);
    grid.get(k).push(i);
  }
  const nn = new Float64Array(n);
  let overlap = 0;
  let purity = 0, purityN = 0;
  for (let i = 0; i < n; i++) {
    let best = Infinity;
    const cand = [];
    for (let rad = 1; rad < 80; rad++) {
      const gx = Math.floor(X(i) / cell), gy = Math.floor(Y(i) / cell);
      cand.length = 0;
      for (let ox = -rad; ox <= rad; ox++) for (let oy = -rad; oy <= rad; oy++) {
        for (const j of grid.get(gx + ox + ',' + (gy + oy)) || []) if (j !== i) cand.push([Math.hypot(X(i) - X(j), Y(i) - Y(j)), j]);
      }
      if (cand.length >= 5) break;
    }
    cand.sort((a, b) => a[0] - b[0]);
    best = cand.length ? cand[0][0] : 0;
    nn[i] = best;
    if (best < 6) overlap++; // two 3px dots would touch
    if (nodes[i].top) {
      const k = cand.slice(0, 5);
      purity += k.filter(([, j]) => nodes[j].top === nodes[i].top).length / Math.max(1, k.length);
      purityN++;
    }
  }
  const sortedNN = [...nn].sort((a, b) => a - b);
  const medNN = sortedNN[n >> 1];
  let len = 0;
  for (const e of edges) len += Math.hypot(X(e.s) - X(e.t), Y(e.s) - Y(e.t));
  const meanLen = len / Math.max(1, edges.length);
  const straight = crossings(edges.map((e) => [P(e.s), P(e.t), e.s, e.t]));
  let chainX = null;
  if (res.chains) {
    // the drawn scope chains + routed edges, as the shipped view draws them
    const segs = [];
    const T = (p) => [(p[0] - x0) * s, (p[1] - y0) * s];
    res.chains.forEach((l, li) => { for (let k = 1; k < l.pts.length; k++) segs.push([T(l.pts[k - 1]), T(l.pts[k]), 'c' + li + ':' + (k - 1), 'c' + li + ':' + (k + 1)]); });
    res.routes.forEach((rt, ei) => { for (let k = 1; k < rt.length; k++) segs.push([T(rt[k - 1]), T(rt[k]), 'e' + ei + ':' + (k - 1), 'e' + ei + ':' + (k + 1)]); });
    chainX = crossings(segs);
  }
  return {
    crossings: straight,
    drawnCrossings: chainX,
    meanEdgePx: meanLen,
    meanEdgeNN: meanLen / Math.max(1e-9, medNN),
    medianNNpx: medNN,
    overlapPct: (overlap / n) * 100,
    scopePurity: (purity / Math.max(1, purityN)) * 100,
    scale: s,
  };
}
