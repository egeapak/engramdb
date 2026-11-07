//! The scope overview's layout: the port of
//! `web/js/graph/scope-overview.js` (`LAYOUTS['scope-overview']`). One disc
//! per top scope, area by its memories, placed by scope affinity with
//! scope-force's [`pack`](super::scope_force::pack), in pixels of a
//! `width` × `width · aspect` box. Same operations in the same order as the
//! JS (see `scope_force`), so the output is the same bits.

// Index loops on purpose: each mirrors a JS `for (let i = 0; …)` loop
// statement for statement, which is what keeps the two bit-identical.
#![allow(clippy::needless_range_loop)]

use serde::Serialize;

use super::scope_force::pack;
use super::{js_cmp, js_round};

const MARGIN: f64 = 14.0;
const LABEL_H: f64 = 18.0;
const CHAR_W: f64 = 7.6;
const MIN_R: f64 = 6.0;
const MAX_R: f64 = 0.3;

/// One disc: a top scope. `full_len` / `short_len` are the UTF-16 lengths
/// of "name · count" and of the name alone.
#[derive(Debug, Clone, Serialize)]
pub struct OverviewNode {
    pub id: String,
    pub count: f64,
    pub full_len: usize,
    pub short_len: usize,
}

/// One related scope pair (`s` ≠ `t`), `n` its relations.
#[derive(Debug, Clone, Serialize)]
pub struct OverviewEdge {
    pub s: usize,
    pub t: usize,
    pub n: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverviewInput {
    pub nodes: Vec<OverviewNode>,
    pub edges: Vec<OverviewEdge>,
    pub aspect: f64,
    pub width: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OverviewLabel {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub anchor: &'static str,
    pub placed: bool,
    pub short: bool,
    pub forced: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OverviewBounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The JS output, field for field (the page uses it as is).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OverviewOutput {
    pub pos: Vec<f64>,
    pub r: Vec<f64>,
    pub routes: Vec<[[f64; 2]; 3]>,
    pub labels: Vec<OverviewLabel>,
    pub bounds: OverviewBounds,
}

pub fn layout(input: &OverviewInput) -> OverviewOutput {
    let nodes = &input.nodes;
    let m = nodes.len();
    let a = if input.aspect.is_nan() || input.aspect == 0.0 {
        0.62
    } else {
        input.aspect
    };
    let aspect = 3f64.min(0.3f64.max(a));
    let w_box = if input.width.is_nan() || input.width == 0.0 {
        800.0
    } else {
        input.width
    };
    let h_box = js_round(w_box * aspect);
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| {
        (nodes[b].count - nodes[a].count)
            .partial_cmp(&0.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| js_cmp(&nodes[a].id, &nodes[b].id))
    });
    let mut rank = vec![0usize; m];
    for (k, &i) in order.iter().enumerate() {
        rank[i] = k;
    }
    let mut rad = vec![0.0f64; m];
    let mut mean = 0.0f64;
    for k in 0..m {
        rad[k] = 1f64.max(nodes[order[k]].count).sqrt();
        mean += rad[k];
    }
    mean = if m > 0 { mean / m as f64 } else { 1.0 };
    let mut aff = vec![0.0f64; m * m];
    for e in &input.edges {
        if e.s == e.t || e.s >= m || e.t >= m {
            continue;
        }
        let a = rank[e.s];
        let b = rank[e.t];
        let n = if e.n != 0.0 && !e.n.is_nan() {
            e.n
        } else {
            1.0
        };
        aff[a * m + b] += n;
        aff[b * m + a] += n;
    }
    let c = pack(&rad, Some(&aff), aspect, 2f64.max(mean * 2.2));
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for k in 0..m {
        x0 = x0.min(c[2 * k] - rad[k]);
        x1 = x1.max(c[2 * k] + rad[k]);
        y0 = y0.min(c[2 * k + 1] - rad[k]);
        y1 = y1.max(c[2 * k + 1] + rad[k]);
    }
    if m == 0 {
        (x0, y0, x1, y1) = (0.0, 0.0, 0.0, 0.0);
    }
    let bw = 1e-6f64.max(x1 - x0);
    let bh = 1e-6f64.max(y1 - y0);
    let mut big = 1.0f64;
    for &v in &rad {
        big = big.max(v);
    }
    let sc = 1e-6f64.max(
        ((MAX_R * w_box.min(h_box)) / big)
            .min((w_box - 2.0 * MARGIN) / bw)
            .min((h_box - 2.0 * MARGIN - 2.0 * LABEL_H) / bh),
    );
    let ox = (w_box - bw * sc) / 2.0 - x0 * sc;
    let oy = (h_box - bh * sc) / 2.0 - y0 * sc;
    let mut pos = vec![0.0f64; 2 * m];
    let mut r = vec![0.0f64; m];
    for i in 0..m {
        let k = rank[i];
        pos[2 * i] = ox + c[2 * k] * sc;
        pos[2 * i + 1] = oy + c[2 * k + 1] * sc;
        r[i] = MIN_R.max(rad[k] * sc);
    }

    let routes = input
        .edges
        .iter()
        .map(|e| {
            let ax = pos[2 * e.s];
            let ay = pos[2 * e.s + 1];
            let tx = pos[2 * e.t];
            let ty = pos[2 * e.t + 1];
            let dx = tx - ax;
            let dy = ty - ay;
            let mut len = (dx * dx + dy * dy).sqrt();
            if len == 0.0 || len.is_nan() {
                len = 1.0;
            }
            let ux = dx / len;
            let uy = dy / len;
            let p0 = [ax + ux * (r[e.s] + 2.0), ay + uy * (r[e.s] + 2.0)];
            let p2 = [tx - ux * (r[e.t] + 2.0), ty - uy * (r[e.t] + 2.0)];
            let bow = 16f64.min(len * 0.06);
            [
                p0,
                [
                    (p0[0] + p2[0]) / 2.0 - uy * bow,
                    (p0[1] + p2[1]) / 2.0 + ux * bow,
                ],
                p2,
            ]
        })
        .collect();

    // Labels, biggest scope first: the first free side of above, below,
    // right, left; then the name alone; then forced above (or below).
    let mut boxes: Vec<[f64; 3]> = Vec::new();
    let free = |boxes: &[[f64; 3]], bx: f64, by: f64, w: f64, own: usize| -> bool {
        if bx < 2.0 || by < 2.0 || bx + w > w_box - 2.0 || by + LABEL_H > h_box - 2.0 {
            return false;
        }
        for q in boxes {
            if bx < q[0] + q[2] && bx + w > q[0] && by < q[1] + LABEL_H && by + LABEL_H > q[1] {
                return false;
            }
        }
        for j in 0..m {
            if j == own {
                continue;
            }
            let nx = bx.max(pos[2 * j].min(bx + w));
            let ny = by.max(pos[2 * j + 1].min(by + LABEL_H));
            let ddx = nx - pos[2 * j];
            let ddy = ny - pos[2 * j + 1];
            if ddx * ddx + ddy * ddy < r[j] * r[j] {
                return false;
            }
        }
        true
    };
    let spots_for = |i: usize, w: f64| -> [(f64, f64, f64, f64, &'static str); 4] {
        let x = pos[2 * i];
        let y = pos[2 * i + 1];
        let rr = r[i];
        [
            (
                x - w / 2.0,
                y - rr - 3.0 - LABEL_H,
                x,
                y - rr - 7.0,
                "middle",
            ),
            (
                x - w / 2.0,
                y + rr + 3.0,
                x,
                y + rr + 3.0 + LABEL_H - 5.0,
                "middle",
            ),
            (
                x + rr + 5.0,
                y - LABEL_H / 2.0,
                x + rr + 8.0,
                y + 4.5,
                "start",
            ),
            (
                x - rr - 5.0 - w,
                y - LABEL_H / 2.0,
                x - rr - 8.0,
                y + 4.5,
                "end",
            ),
        ]
    };
    /// A candidate spot: x, y, rect x, rect y, anchor.
    type Spot = (f64, f64, f64, f64, &'static str);
    let width_of = |len: usize| (len as f64 * CHAR_W).ceil() + 6.0;
    let mut labels: Vec<Option<OverviewLabel>> = vec![None; m];
    for &i in &order {
        let nd = &nodes[i];
        let mut done: Option<(Spot, f64, bool, bool)> = None;
        'texts: for (len, short) in [(nd.full_len, false), (nd.short_len, true)] {
            let w = width_of(len);
            for sp in spots_for(i, w) {
                if free(&boxes, sp.0, sp.1, w, i) {
                    done = Some((sp, w, short, false));
                    break 'texts;
                }
            }
        }
        let (sp, w, short, forced) = done.unwrap_or_else(|| {
            let w = width_of(nd.full_len);
            let s = spots_for(i, w);
            (if s[0].1 >= 2.0 { s[0] } else { s[1] }, w, false, true)
        });
        boxes.push([sp.0, sp.1, w]);
        labels[i] = Some(OverviewLabel {
            x: sp.2,
            y: sp.3,
            w,
            anchor: sp.4,
            placed: true,
            short,
            forced,
        });
    }
    OverviewOutput {
        pos,
        r,
        routes,
        labels: labels.into_iter().map(|l| l.expect("labelled")).collect(),
        bounds: OverviewBounds {
            x: 0.0,
            y: 0.0,
            w: w_box,
            h: h_box,
        },
    }
}
