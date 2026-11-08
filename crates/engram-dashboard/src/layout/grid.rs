//! The Grid style's layout: a seeded force simulation snapped to a grid,
//! with collision-free labels. Ported from the page's former
//! `web/js/graph/themes/grid/layout.js` operation for operation (the same
//! double operations in the same order, `Math.hypot` as V8 computes it
//! ([`hypot`]), the sixteen coincident-pair directions as the bits V8's
//! `Math.cos` / `Math.sin` gave them ([`DIR16`])), so a store draws exactly
//! the map it drew when the page laid it out (screenshot-diffed at 0 %
//! when the JS was retired). The JS is gone; these rules are what keep the
//! layout deterministic now.
//!
//! Steps:
//!   1. Groups (top scopes) start as compact blocks, each group's memories
//!      in a serpentine so consecutive ones are neighbours; blocks are
//!      shelf-packed to the box's aspect ratio, so Fit uses the whole box.
//!   2. A force simulation: soft repulsion through a spatial hash, springs
//!      along relations, stronger springs along each group's chain, weak
//!      gravity to the group's block, under a cooling step cap.
//!   3. Snap: memories claim points of a grid wider than it is tall (labels
//!      are wider than they are tall), most connected first, each taking
//!      the nearest free point.
//!   4. Chains: each group visits its memories in a nearest-neighbour order
//!      from one end, so it does not double back across the map.
//!   5. Routing: chains and relations become polylines of horizontal,
//!      vertical and diagonal segments (straight–diagonal–straight for
//!      chains, diagonal–straight for relations).
//!   6. Labels: [`labels::place_labels`] with each chain's badge as an
//!      obstacle.
//!
//! Deterministic: the only randomness is a PRNG seeded from the memory ids,
//! and every loop walks memories in id order. Positions depend on the nodes
//! and edges given and nothing else, and not on the box's width.

// Index loops on purpose: each follows the original loop statement for
// statement, which is what keeps the output the same bits.
#![allow(clippy::needless_range_loop)]

use std::collections::{HashMap, HashSet};

use super::labels::{self, place_labels, LabelNode, Options};
use super::{js_cmp, js_round, ForceInput, Rect};

pub const GRID: f64 = 48.0;
/// Grid pitch after snapping: columns are wider than rows (labels).
pub const GX: f64 = 104.0;
pub const GY: f64 = 60.0;

/// `Math.cos(k * (Math.PI / 8))`, `Math.sin(…)` for k = 0..15, as V8
/// computes them (bits): the coincident-pair directions.
const DIR16: [(u64, u64); 16] = [
    (0x3ff0000000000000, 0x0),
    (0x3fed906bcf328d46, 0x3fd87de2a6aea963),
    (0x3fe6a09e667f3bcd, 0x3fe6a09e667f3bcc),
    (0x3fd87de2a6aea964, 0x3fed906bcf328d46),
    (0x3c91a62633145c07, 0x3ff0000000000000),
    (0xbfd87de2a6aea962, 0x3fed906bcf328d46),
    (0xbfe6a09e667f3bcc, 0x3fe6a09e667f3bcd),
    (0xbfed906bcf328d46, 0x3fd87de2a6aea965),
    (0xbff0000000000000, 0x3ca1a62633145c07),
    (0xbfed906bcf328d47, 0xbfd87de2a6aea961),
    (0xbfe6a09e667f3bce, 0xbfe6a09e667f3bcc),
    (0xbfd87de2a6aea96d, 0xbfed906bcf328d44),
    (0xbcaa79394c9e8a0a, 0xbff0000000000000),
    (0x3fd87de2a6aea967, 0xbfed906bcf328d45),
    (0x3fe6a09e667f3bcb, 0xbfe6a09e667f3bce),
    (0x3fed906bcf328d44, 0xbfd87de2a6aea96e),
];

/// `Math.hypot(a, b)` as V8 computes it: scaled by the larger magnitude,
/// Kahan-summed, `sqrt` times the scale (not `f64::hypot`, which rounds
/// differently).
pub fn hypot(a: f64, b: f64) -> f64 {
    let (x, y) = (a.abs(), b.abs());
    if x == f64::INFINITY || y == f64::INFINITY {
        return f64::INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    let max = x.max(y);
    if max == 0.0 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut comp = 0.0f64;
    for v in [x, y] {
        let n = v / max;
        let s = n * n - comp;
        let p = sum + s;
        comp = (p - sum) - s;
        sum = p;
    }
    sum.sqrt() * max
}

/// FNV-1a over UTF-16 code units (32-bit).
fn fnv(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for c in s.encode_utf16() {
        h ^= u32::from(c);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// mulberry32, as the JS wrote it.
struct Prng(i32);

impl Prng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let a = self.0;
        let mut t = (a ^ ((a as u32) >> 15) as i32).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ ((t as u32) >> 7) as i32).wrapping_mul(61 | t)) ^ t;
        f64::from((t ^ ((t as u32) >> 14) as i32) as u32) / 4_294_967_296.0
    }
}

/// One group (a top scope) and its chain of memories.
struct Line {
    key: String,
    slot: Option<u32>,
    members: Vec<usize>,
    cx: f64,
    cy: f64,
}

/// A group's drawn chain.
#[derive(Debug, Clone, PartialEq)]
pub struct LineOut {
    pub key: String,
    pub slot: Option<u32>,
    pub pts: Vec<[f64; 2]>,
}

/// The layout's output.
#[derive(Debug, Clone, PartialEq)]
pub struct GridOutput {
    pub pos: Vec<f64>,
    pub routes: Vec<Vec<[f64; 2]>>,
    pub lines: Vec<LineOut>,
    pub labels: Vec<labels::Label>,
    /// x, y, w, h.
    pub bounds: [f64; 4],
}

fn lines_of(input: &ForceInput, order: &[usize]) -> Vec<Line> {
    let nodes = &input.nodes;
    let mut lines: Vec<Line> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for &i in order {
        let key = nodes[i].top_scope.as_str();
        let k = *at.entry(key).or_insert_with(|| {
            lines.push(Line {
                key: key.to_string(),
                slot: nodes[i].slot,
                members: Vec::new(),
                cx: 0.0,
                cy: 0.0,
            });
            lines.len() - 1
        });
        lines[k].members.push(i);
    }
    lines.sort_by(|a, b| {
        a.slot
            .is_none()
            .cmp(&b.slot.is_none())
            .then_with(|| a.slot.unwrap_or(0).cmp(&b.slot.unwrap_or(0)))
            .then_with(|| js_cmp(&a.key, &b.key))
    });
    for l in &mut lines {
        l.members.sort_by(|&a, &b| {
            js_cmp(&nodes[a].scope, &nodes[b].scope)
                .then_with(|| js_cmp(&nodes[a].id, &nodes[b].id))
        });
    }
    lines
}

/// Step 1: serpentine blocks, shelf-packed.
fn seed(lines: &mut [Line], rand: &mut Prng, px: &mut [f64], py: &mut [f64], aspect: f64) {
    let step = GRID * 1.5;
    struct Block {
        cols: usize,
        w: f64,
        h: f64,
    }
    let blocks: Vec<Block> = lines
        .iter()
        .map(|l| {
            let m = l.members.len();
            let cols = 1usize.max(m.min((m as f64 * 2.5).sqrt().ceil() as usize));
            let rows = (m as f64 / cols as f64).ceil();
            Block {
                cols,
                w: cols as f64 * step + GRID * 2.0,
                h: rows * step + GRID * 2.0,
            }
        })
        .collect();
    let mut area = 0.0f64;
    let mut widest = 0.0f64;
    for b in &blocks {
        area += b.w * b.h;
        widest = widest.max(b.w);
    }
    let a = if aspect == 0.0 || aspect.is_nan() {
        0.6
    } else {
        aspect
    };
    let sim_aspect = 4f64.min(0.2f64.max(a * (GX / GY)));
    let width = widest.max((area / sim_aspect).sqrt());
    let (mut x, mut y, mut shelf) = (0.0f64, 0.0f64, 0.0f64);
    for (l, b) in lines.iter_mut().zip(&blocks) {
        if x > 0.0 && x + b.w > width {
            x = 0.0;
            y += shelf;
            shelf = 0.0;
        }
        let (bx, by) = (x, y);
        x += b.w;
        shelf = shelf.max(b.h);
        for (k, &i) in l.members.iter().enumerate() {
            let r = (k / b.cols) as f64;
            let c0 = k % b.cols;
            let c = if (k / b.cols) % 2 == 0 {
                c0
            } else {
                b.cols - 1 - c0
            } as f64;
            px[i] = bx + GRID + c * step + (rand.next() - 0.5) * GRID * 0.4;
            py[i] = by + GRID + r * step + (rand.next() - 0.5) * GRID * 0.4;
        }
        l.cx = bx + b.w / 2.0;
        l.cy = by + b.h / 2.0;
    }
}

/// `Math.floor(v / cell)` as a bucket coordinate (`-0` and `0` alike, as
/// the JS string key made them).
fn cell_of(v: f64, cell: f64) -> i64 {
    (v / cell).floor() as i64
}

/// Step 2: the force simulation.
fn simulate(input: &ForceInput, lines: &[Line], px: &mut [f64], py: &mut [f64]) {
    let n = input.nodes.len();
    let iters = if n <= 150 {
        260
    } else if n <= 600 {
        160
    } else {
        90
    };
    let r_reach = GRID * 1.9;
    let cell = r_reach;
    let mut line_of = vec![0usize; n];
    for (li, l) in lines.iter().enumerate() {
        for &i in &l.members {
            line_of[i] = li;
        }
    }
    let mut dx = vec![0.0f64; n];
    let mut dy = vec![0.0f64; n];
    let spring = |px: &[f64],
                  py: &[f64],
                  dx: &mut [f64],
                  dy: &mut [f64],
                  a: usize,
                  b: usize,
                  rest: f64,
                  k: f64| {
        let ddx = px[b] - px[a];
        let ddy = py[b] - py[a];
        let h = hypot(ddx, ddy);
        let d = if h == 0.0 || h.is_nan() { 0.01 } else { h };
        let f = (d - rest) * k;
        dx[a] += (ddx / d) * f;
        dy[a] += (ddy / d) * f;
        dx[b] -= (ddx / d) * f;
        dy[b] -= (ddy / d) * f;
    };
    for it in 0..iters {
        let temp = GRID * (0.6 - (0.58 * it as f64) / iters as f64);
        dx.fill(0.0);
        dy.fill(0.0);
        let mut buckets: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for i in 0..n {
            buckets
                .entry((cell_of(px[i], cell), cell_of(py[i], cell)))
                .or_default()
                .push(i);
        }
        for i in 0..n {
            let cx = cell_of(px[i], cell);
            let cy = cell_of(py[i], cell);
            for ox in -1..=1 {
                for oy in -1..=1 {
                    let Some(b) = buckets.get(&(cx + ox, cy + oy)) else {
                        continue;
                    };
                    for &j in b {
                        if j <= i {
                            continue;
                        }
                        let mut ddx = px[i] - px[j];
                        let mut ddy = py[i] - py[j];
                        let mut d = hypot(ddx, ddy);
                        if d >= r_reach {
                            continue;
                        }
                        if d < 0.01 {
                            let (c, s) = DIR16[(i * 7 + j * 13) % 16];
                            ddx = f64::from_bits(c);
                            ddy = f64::from_bits(s);
                            d = 1.0;
                        }
                        let f = ((r_reach - d) / r_reach) * 0.5;
                        dx[i] += (ddx / d) * f * GRID * 0.5;
                        dy[i] += (ddy / d) * f * GRID * 0.5;
                        dx[j] -= (ddx / d) * f * GRID * 0.5;
                        dy[j] -= (ddy / d) * f * GRID * 0.5;
                    }
                }
            }
        }
        for e in &input.edges {
            if e.s >= 0 && e.t >= 0 && e.t != e.s {
                spring(
                    px,
                    py,
                    &mut dx,
                    &mut dy,
                    e.s as usize,
                    e.t as usize,
                    GRID * 2.2,
                    0.03,
                );
            }
        }
        for l in lines {
            for k in 1..l.members.len() {
                spring(
                    px,
                    py,
                    &mut dx,
                    &mut dy,
                    l.members[k - 1],
                    l.members[k],
                    GRID * 1.4,
                    0.08,
                );
            }
        }
        for i in 0..n {
            let l = &lines[line_of[i]];
            dx[i] += (l.cx - px[i]) * 0.004;
            dy[i] += (l.cy - py[i]) * 0.004;
            let m = hypot(dx[i], dy[i]);
            let s = if m > temp { temp / m } else { 1.0 };
            px[i] += dx[i] * s;
            py[i] += dy[i] * s;
        }
    }
}

/// Step 3: every memory claims the nearest free point of the GX × GY grid.
fn snap(input: &ForceInput, px: &mut [f64], py: &mut [f64], aspect: f64) -> HashSet<(i64, i64)> {
    let nodes = &input.nodes;
    let n = nodes.len();
    let (mut x0, mut x1) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
    for i in 0..n {
        x0 = x0.min(px[i]);
        x1 = x1.max(px[i]);
        y0 = y0.min(py[i]);
        y1 = y1.max(py[i]);
    }
    let sw = GRID.max(x1 - x0);
    let sh = GRID.max(y1 - y0);
    let a = if aspect == 0.0 || aspect.is_nan() {
        0.6
    } else {
        aspect
    };
    let q = 3f64.min((1.0 / 3.0f64).max((a * sw * GX) / (sh * GY)));
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        (nodes[b].degree - nodes[a].degree)
            .partial_cmp(&0.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| js_cmp(&nodes[a].id, &nodes[b].id))
    });
    let mut taken: HashSet<(i64, i64)> = HashSet::new();
    let k = 1.0 / (GRID * 1.15);
    let kx = if n > 2 { k / q.sqrt() } else { k };
    let ky = if n > 2 { k * q.sqrt() } else { k };
    for &i in &order {
        let fx = px[i] * kx;
        let fy = py[i] * ky;
        let gx0 = js_round(fx) as i64;
        let gy0 = js_round(fy) as i64;
        let mut best: Option<(i64, i64)> = None;
        let mut ring = 0i64;
        while ring < 400 && best.is_none() {
            let mut bd = f64::INFINITY;
            for ox in -ring..=ring {
                for oy in -ring..=ring {
                    if ox.abs().max(oy.abs()) != ring {
                        continue;
                    }
                    let gx = gx0 + ox;
                    let gy = gy0 + oy;
                    if taken.contains(&(gx, gy)) {
                        continue;
                    }
                    let ex = gx as f64 - fx;
                    let ey = gy as f64 - fy;
                    let d = ex * ex + ey * ey;
                    if d < bd {
                        bd = d;
                        best = Some((gx, gy));
                    }
                }
            }
            ring += 1;
        }
        // 400 rings always hold a free point for a store's memories.
        let (gx, gy) = best.unwrap_or((gx0, gy0));
        taken.insert((gx, gy));
        px[i] = gx as f64 * GX;
        py[i] = gy as f64 * GY;
    }
    taken
}

/// Step 4: order each group's memories as a path from one end.
fn order_lines(lines: &mut [Line], px: &[f64], py: &[f64]) {
    for l in lines {
        let m = &l.members;
        if m.len() < 3 {
            continue;
        }
        let (mut cx, mut cy) = (0.0f64, 0.0f64);
        for &i in m {
            cx += px[i];
            cy += py[i];
        }
        cx /= m.len() as f64;
        cy /= m.len() as f64;
        let mut start = m[0];
        let mut far = -1.0f64;
        for &i in m {
            let ex = px[i] - cx;
            let ey = py[i] - cy;
            let d = ex * ex + ey * ey;
            if d > far + 1e-9 {
                far = d;
                start = i;
            }
        }
        // A JS Set: insertion order, deletions kept out.
        let mut left: Vec<usize> = m.iter().copied().filter(|&i| i != start).collect();
        let mut path = vec![start];
        let mut cur = start;
        while !left.is_empty() {
            let mut best: i64 = -1;
            let mut at = 0usize;
            let mut bd = f64::INFINITY;
            for (p, &j) in left.iter().enumerate() {
                let ex = px[j] - px[cur];
                let ey = py[j] - py[cur];
                let d = ex * ex + ey * ey;
                if d < bd - 1e-9 || ((d - bd).abs() <= 1e-9 && (j as i64) < best) {
                    bd = d;
                    best = j as i64;
                    at = p;
                }
            }
            let j = left.remove(at);
            path.push(j);
            cur = j;
        }
        l.members = path;
    }
}

/// Each chain's badge (the Grid backdrop draws it 24 × 24, up and left of
/// its first memory): a label obstacle.
fn badge_boxes(lines: &[Line], px: &[f64], py: &[f64]) -> Vec<Rect> {
    lines
        .iter()
        .filter(|l| !l.key.is_empty() && !l.members.is_empty())
        .map(|l| {
            let i = l.members[0];
            Rect {
                x0: px[i] - 38.0,
                y0: py[i] - 36.0,
                x1: px[i] - 10.0,
                y1: py[i] - 8.0,
            }
        })
        .collect()
}

fn sign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// Straight–diagonal–straight between two points (chains).
fn line_leg(a: [f64; 2], b: [f64; 2]) -> Vec<[f64; 2]> {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let ax = dx.abs();
    let ay = dy.abs();
    if ax == 0.0 || ay == 0.0 || ax == ay {
        return vec![b];
    }
    if ax > ay {
        let run = (ax - ay) / 2.0;
        return vec![
            [a[0] + sign(dx) * run, a[1]],
            [b[0] - sign(dx) * run, b[1]],
            b,
        ];
    }
    let run = (ay - ax) / 2.0;
    vec![
        [a[0], a[1] + sign(dy) * run],
        [b[0], b[1] - sign(dy) * run],
        b,
    ]
}

/// Diagonal then straight (relations).
fn edge_route(a: [f64; 2], b: [f64; 2]) -> Vec<[f64; 2]> {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let ax = dx.abs();
    let ay = dy.abs();
    if ax == 0.0 || ay == 0.0 || ax == ay {
        return vec![a, b];
    }
    let d = ax.min(ay);
    vec![a, [a[0] + sign(dx) * d, a[1] + sign(dy) * d], b]
}

const DIRS: [[f64; 2]; 8] = [
    [1.0, 0.0],
    [0.0, -1.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [1.0, -1.0],
    [-1.0, -1.0],
    [-1.0, 1.0],
    [1.0, 1.0],
];

/// The grid point `(gx, gy)` is taken (a non-integral coordinate names no
/// grid point, as the JS string key did not).
fn is_taken(taken: &HashSet<(i64, i64)>, gx: f64, gy: f64) -> bool {
    gx.fract() == 0.0 && gy.fract() == 0.0 && taken.contains(&(gx as i64, gy as i64))
}

/// The whole layout.
pub fn layout(input: &ForceInput) -> GridOutput {
    let nodes = &input.nodes;
    let n = nodes.len();
    let mut px = vec![0.0f64; n];
    let mut py = vec![0.0f64; n];
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| js_cmp(&nodes[a].id, &nodes[b].id));
    let ids: Vec<&str> = order.iter().map(|&i| nodes[i].id.as_str()).collect();
    let mut rand = Prng(fnv(&ids.join("|")) as i32);
    let mut lines = lines_of(input, &order);
    seed(&mut lines, &mut rand, &mut px, &mut py, input.aspect);
    if n > 1 {
        simulate(input, &lines, &mut px, &mut py);
    }
    let taken = snap(input, &mut px, &mut py, input.aspect);
    order_lines(&mut lines, &px, &py);
    let mut flat = vec![0.0f64; 2 * n];
    for i in 0..n {
        flat[2 * i] = px[i];
        flat[2 * i + 1] = py[i];
    }
    let label_nodes: Vec<LabelNode<'_>> = nodes
        .iter()
        .map(|nd| LabelNode {
            id: &nd.id,
            r: nd.r,
            label_len: nd.label_len,
            degree: nd.degree,
            weight: nd.weight,
        })
        .collect();
    let obstacles = badge_boxes(&lines, &px, &py);
    let mut label_pos = flat.clone();
    let labels = place_labels(
        &label_nodes,
        &mut label_pos,
        &obstacles,
        Options {
            corners: false,
            nudge: 0.0,
            test: None,
        },
    );

    let p = |i: usize| [px[i], py[i]];
    let routes: Vec<Vec<[f64; 2]>> = input
        .edges
        .iter()
        .enumerate()
        .map(|(k, e)| {
            if e.s < 0 {
                // Never sent (every relation's source is drawn): a
                // degenerate route rather than a panic.
                return vec![[0.0, 0.0], [0.0, 0.0]];
            }
            let s = e.s as usize;
            if e.t >= 0 {
                return edge_route(p(s), p(e.t as usize));
            }
            // A stub: the first free neighbouring direction, in an
            // edge-derived order.
            let gx = px[s] / GX;
            let gy = py[s] / GY;
            let start = (fnv(&format!("{k}{}", nodes[s].id)) % 8) as usize;
            let mut dir = DIRS[start];
            for j in 0..8 {
                let d = DIRS[(start + j) % 8];
                if !is_taken(&taken, gx + d[0], gy + d[1]) {
                    dir = d;
                    break;
                }
            }
            let len = GY * 0.75;
            let norm = hypot(dir[0], dir[1]);
            vec![
                p(s),
                [px[s] + (dir[0] / norm) * len, py[s] + (dir[1] / norm) * len],
            ]
        })
        .collect();
    let line_out: Vec<LineOut> = lines
        .iter()
        .map(|l| {
            let mut pts: Vec<[f64; 2]> = Vec::new();
            for (k, &i) in l.members.iter().enumerate() {
                if k == 0 {
                    pts.push(p(i));
                } else {
                    let last = *pts.last().expect("a first point");
                    pts.extend(line_leg(last, p(i)));
                }
            }
            LineOut {
                key: l.key.clone(),
                slot: l.slot,
                pts,
            }
        })
        .collect();

    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut grow = |x: f64, y: f64| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for i in 0..n {
        let r = (if nodes[i].r != 0.0 && !nodes[i].r.is_nan() {
            nodes[i].r
        } else {
            6.0
        }) + 6.0;
        grow(px[i] - r, py[i] - r);
        grow(px[i] + r, py[i] + r);
    }
    for r in &routes {
        for pt in r {
            grow(pt[0], pt[1]);
        }
    }
    for l in &labels {
        if !l.placed {
            continue;
        }
        grow(l.rect.x0, l.rect.y0);
        grow(l.rect.x1, l.rect.y1);
    }
    for l in &line_out {
        if !l.key.is_empty() && !l.pts.is_empty() {
            grow(l.pts[0][0] - 40.0, l.pts[0][1] - 36.0);
        }
    }
    if n == 0 {
        (min_x, min_y, max_x, max_y) = (0.0, 0.0, GRID, GRID);
    }
    let pad = 16.0;
    GridOutput {
        pos: flat,
        routes,
        lines: line_out,
        labels,
        bounds: [
            min_x - pad,
            min_y - pad,
            max_x - min_x + pad * 2.0,
            max_y - min_y + pad * 2.0,
        ],
    }
}
