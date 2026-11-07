//! The scope-aware force layout: the port of `web/js/graph/scope-force.js`
//! (`LAYOUTS['scope-force']`, Plain's layout and every opened scope's).
//!
//! The steps, the constants and the reasons for them are documented in the
//! JS module, which stays the reference (and the page's fallback when the
//! server sends no layout). This file follows it statement for statement,
//! with the same floating-point operations in the same order, so that a
//! given input gives the same bits in both: JS numbers are IEEE doubles,
//! `+ − × ÷` and `sqrt` are correctly rounded in both languages, Rust never
//! fuses a multiply-add on its own, and every loop walks memories in the
//! same (id) order and maps in the same (insertion) order as the JS. Where
//! JS keys a `Map` by a computed number (`cx * 65536 + cy`) this keys by
//! the same double, so even its collisions match.

// Index loops on purpose: each mirrors a JS `for (let i = 0; …)` loop
// statement for statement, which is what keeps the two bit-identical.
#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;

use super::labels::{place_labels, LabelNode, Nudge, Options};
use super::{
    fkey, js_cmp, js_round, CoreTop, ForceCore, ForceInput, ForceOutput, Rect, ScopeNameOut,
};

/// The ideal relation length, in layout units.
const S: f64 = 30.0;
const CUT: f64 = 6.0 * S;
const CELL: f64 = 2.0 * S;
const SEED: f64 = 0.56 * S;
const GA_C: f64 = -0.7373688780783197;
const GA_S: f64 = 0.6754902942615238;
const R3: f64 = 0.8660254037844386;
const DIRS: [(f64, f64); 12] = [
    (1.0, 0.0),
    (R3, 0.5),
    (0.5, R3),
    (0.0, 1.0),
    (-0.5, R3),
    (-R3, 0.5),
    (-1.0, 0.0),
    (-R3, -0.5),
    (-0.5, -R3),
    (0.0, -1.0),
    (0.5, -R3),
    (R3, -0.5),
];
const PACK_MAX: usize = 100;
const TURN_MAX: usize = 96;
const SPREAD_TO: f64 = 860.0;
const NUDGE_MAX: usize = 300;
const NUDGE_DEGREE: usize = 24;
const NUDGE_WORK: u64 = 300000;
const GRID_MAX: usize = 1024;

/// The layout options a style may pass (`layout.options`); the server runs
/// the defaults only (Plain and the opened scope pass `{}`).
#[derive(Debug, Clone, Copy)]
pub struct ForceOptions {
    pub top_pull: f64,
    pub sub_pull: f64,
    pub gravity: f64,
    pub repulsion: f64,
    pub steps: f64,
    pub temp: f64,
}

impl Default for ForceOptions {
    fn default() -> Self {
        Self {
            top_pull: 0.025,
            sub_pull: 0.02,
            gravity: 0.008,
            repulsion: 0.2,
            steps: 1.0,
            temp: 0.9,
        }
    }
}

/// `Math.min(hi, Math.max(lo, v))` for finite numbers.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    hi.min(lo.max(v))
}

// ---------------------------------------------------------------------------
// Start positions
// ---------------------------------------------------------------------------

/// `pack(rad, aff, aspect, pad)`: disc centres `[x0, y0, …]`.
pub fn pack(rad: &[f64], aff: Option<&[f64]>, aspect: f64, pad: f64) -> Vec<f64> {
    let m = rad.len();
    let mut out = vec![0.0f64; m * 2];
    if m <= 1 {
        return out;
    }
    let gx = aspect;
    let gy = 1.0 / aspect;
    if m > PACK_MAX {
        let mut c = 1.0f64;
        let mut sn = 0.0f64;
        let mut avg = 0.0f64;
        for &v in rad {
            avg += v;
        }
        avg /= m as f64;
        for j in 0..m {
            let rr = avg * 2.1 * (j as f64).sqrt();
            out[2 * j] = (rr * c) / aspect.sqrt();
            out[2 * j + 1] = rr * sn * aspect.sqrt();
            let nc = c * GA_C - sn * GA_S;
            sn = c * GA_S + sn * GA_C;
            c = nc;
        }
        return out;
    }
    let mut placed = vec![false; m];
    let mut to_placed = vec![0.0f64; m];
    let mut order: Vec<usize> = vec![0];
    placed[0] = true;
    if let Some(aff) = aff {
        for (j, tp) in to_placed.iter_mut().enumerate() {
            *tp += aff[j * m];
        }
    }
    for _ in 1..m {
        let mut j: Option<usize> = None;
        for q in 0..m {
            if !placed[q] && j.is_none_or(|j| to_placed[q] > to_placed[j]) {
                j = Some(q);
            }
        }
        let j = j.expect("an unplaced disc");
        let mut total = 0.0f64;
        if let Some(aff) = aff {
            for &q in &order {
                total += aff[j * m + q];
            }
        }
        let mut best = f64::INFINITY;
        let mut bx = 0.0;
        let mut by = 0.0;
        let mut anchors: Vec<usize> = order.clone();
        if order.len() > 36 {
            anchors = order[order.len() - 24..].to_vec();
            if let Some(aff) = aff {
                let mut rel: Vec<usize> = order[..order.len() - 24]
                    .iter()
                    .copied()
                    .filter(|&q| aff[j * m + q] > 0.0)
                    .collect();
                rel.sort_by(|&p, &q| {
                    let d = aff[j * m + q] - aff[j * m + p];
                    if d != 0.0 && !d.is_nan() {
                        d.partial_cmp(&0.0).unwrap()
                    } else {
                        p.cmp(&q)
                    }
                });
                anchors.extend(rel.into_iter().take(12));
            }
        }
        for &p in &anchors {
            let reach = rad[p] + rad[j] + pad;
            for &(dx, dy) in &DIRS {
                let cx = out[2 * p] + dx * reach;
                let cy = out[2 * p + 1] + dy * reach;
                let mut ok = true;
                let mut cost = 0.0f64;
                for &q in &order {
                    let ex = cx - out[2 * q];
                    let ey = cy - out[2 * q + 1];
                    let d2 = ex * ex + ey * ey;
                    let min = rad[q] + rad[j] + pad - 1e-6;
                    if d2 < min * min {
                        ok = false;
                        break;
                    }
                    if total > 0.0 {
                        let w = aff.expect("total > 0 needs affinities")[j * m + q];
                        if w != 0.0 {
                            cost += (w / total) * d2.sqrt();
                        }
                    }
                }
                if !ok {
                    continue;
                }
                cost += (cx * cx * gx + cy * cy * gy).sqrt();
                if cost < best - 1e-9 {
                    best = cost;
                    bx = cx;
                    by = cy;
                }
            }
        }
        out[2 * j] = bx;
        out[2 * j + 1] = by;
        placed[j] = true;
        order.push(j);
        if let Some(aff) = aff {
            for (q, tp) in to_placed.iter_mut().enumerate() {
                *tp += aff[q * m + j];
            }
        }
    }
    out
}

fn sunflower(ids: &[usize], x: &mut [f64], y: &mut [f64], cx: f64, cy: f64, sp: f64) {
    let mut c = 1.0f64;
    let mut sn = 0.0f64;
    for (k, &id) in ids.iter().enumerate() {
        let rr = sp * (k as f64 + 0.5).sqrt();
        x[id] = cx + rr * c;
        y[id] = cy + rr * sn;
        let nc = c * GA_C - sn * GA_S;
        sn = c * GA_S + sn * GA_C;
        c = nc;
    }
}

// ---------------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------------

struct Sim<'a> {
    n: usize,
    x: &'a mut [f64],
    y: &'a mut [f64],
    ea: &'a [usize],
    eb: &'a [usize],
    mass: &'a [f64],
    top_of: &'a [usize],
    top_w: &'a [f64],
    sub_of: &'a [Option<usize>],
    fx: Vec<f64>,
    fy: Vec<f64>,
    tx: Vec<f64>,
    ty: Vec<f64>,
    tm: Vec<f64>,
    sx: Vec<f64>,
    sy: Vec<f64>,
    sm: Vec<f64>,
    cell_of: Vec<usize>,
    items: Vec<usize>,
    start: Vec<usize>,
    fill: Vec<usize>,
    cmass: Vec<f64>,
    cmx: Vec<f64>,
    cmy: Vec<f64>,
    far_x: Vec<f64>,
    far_y: Vec<f64>,
    gw: usize,
    gh: usize,
    rep: f64,
    top_pull: f64,
    sub_pull: f64,
    ax: f64,
    ay: f64,
}

impl Sim<'_> {
    fn bin(&mut self) {
        let n = self.n;
        let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
        let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for i in 0..n {
            let (xi, yi) = (self.x[i], self.y[i]);
            if xi < x0 {
                x0 = xi;
            }
            if xi > x1 {
                x1 = xi;
            }
            if yi < y0 {
                y0 = yi;
            }
            if yi > y1 {
                y1 = yi;
            }
        }
        let gw = GRID_MAX.min(((x1 - x0) / CELL).floor() as usize + 1);
        let gh = GRID_MAX.min(((y1 - y0) / CELL).floor() as usize + 1);
        let cells = gw * gh;
        self.gw = gw;
        self.gh = gh;
        if self.start.len() < cells + 1 {
            self.start = vec![0; cells + 1];
            self.fill = vec![0; cells + 1];
            self.cmass = vec![0.0; cells];
            self.cmx = vec![0.0; cells];
            self.cmy = vec![0.0; cells];
            self.far_x = vec![0.0; cells];
            self.far_y = vec![0.0; cells];
        } else {
            self.start[..=cells].fill(0);
            self.cmass[..cells].fill(0.0);
            self.cmx[..cells].fill(0.0);
            self.cmy[..cells].fill(0.0);
        }
        for i in 0..n {
            let cx = (gw - 1).min(((self.x[i] - x0) / CELL).floor() as usize);
            let cy = (gh - 1).min(((self.y[i] - y0) / CELL).floor() as usize);
            let c = cy * gw + cx;
            self.cell_of[i] = c;
            self.start[c + 1] += 1;
            self.cmass[c] += self.mass[i];
            self.cmx[c] += self.x[i] * self.mass[i];
            self.cmy[c] += self.y[i] * self.mass[i];
        }
        for c in 0..cells {
            self.start[c + 1] += self.start[c];
            self.fill[c] = self.start[c];
            if self.cmass[c] > 0.0 {
                self.cmx[c] /= self.cmass[c];
                self.cmy[c] /= self.cmass[c];
            }
        }
        for i in 0..n {
            let c = self.cell_of[i];
            self.items[self.fill[c]] = i;
            self.fill[c] += 1;
        }
    }

    fn far_field(&mut self) {
        let (gw, gh) = (self.gw as i64, self.gh as i64);
        let cells = self.gw * self.gh;
        let k2 = S * S;
        let cut2 = CUT * CUT;
        let too_near = (CELL * CELL) / 4.0;
        for c in 0..cells {
            self.far_x[c] = 0.0;
            self.far_y[c] = 0.0;
            if self.cmass[c] == 0.0 {
                continue;
            }
            let ci = c as i64 % gw;
            let cj = (c as i64 - ci) / gw;
            let xc = self.cmx[c];
            let yc = self.cmy[c];
            let mut ux = 0.0f64;
            let mut uy = 0.0f64;
            let r0 = if cj > 2 { cj - 3 } else { 0 };
            let r1 = if cj < gh - 3 { cj + 3 } else { gh - 1 };
            let c0 = if ci > 2 { ci - 3 } else { 0 };
            let c1 = if ci < gw - 3 { ci + 3 } else { gw - 1 };
            for rj in r0..=r1 {
                let near = rj >= cj - 1 && rj <= cj + 1;
                for ri in c0..=c1 {
                    if near && ri >= ci - 1 && ri <= ci + 1 {
                        continue;
                    }
                    let cc = (rj * gw + ri) as usize;
                    let m = self.cmass[cc];
                    if m == 0.0 {
                        continue;
                    }
                    let dx = xc - self.cmx[cc];
                    let dy = yc - self.cmy[cc];
                    let d2 = dx * dx + dy * dy;
                    if d2 >= cut2 || d2 < too_near {
                        continue;
                    }
                    let tp = 1.0 - d2.sqrt() / CUT;
                    let f = (k2 * m * tp * tp) / d2;
                    ux += dx * f;
                    uy += dy * f;
                }
            }
            self.far_x[c] = ux;
            self.far_y[c] = uy;
        }
    }

    fn near_field(&mut self) {
        let (gw, gh) = (self.gw, self.gh);
        let k2 = S * S;
        for i in 0..self.n {
            let c = self.cell_of[i];
            let ci = c % gw;
            let cj = (c - ci) / gw;
            let xi = self.x[i];
            let yi = self.y[i];
            let mut ux = self.far_x[c];
            let mut uy = self.far_y[c];
            let r0 = if cj > 0 { cj - 1 } else { 0 };
            let r1 = if cj < gh - 1 { cj + 1 } else { gh - 1 };
            let c0 = if ci > 0 { ci - 1 } else { 0 };
            let c1 = if ci < gw - 1 { ci + 1 } else { gw - 1 };
            for rj in r0..=r1 {
                let end = self.start[rj * gw + c1 + 1];
                for q in self.start[rj * gw + c0]..end {
                    let j = self.items[q];
                    if j == i {
                        continue;
                    }
                    let mut dx = xi - self.x[j];
                    let mut dy = yi - self.y[j];
                    let mut d2 = dx * dx + dy * dy;
                    if d2 < 0.01 {
                        let d = DIRS[(i * 7 + j * 5) % 12];
                        let s = if i < j { -0.1 } else { 0.1 };
                        dx = d.0 * s;
                        dy = d.1 * s;
                        d2 = 0.01;
                    }
                    let tp = 1.0 - d2.sqrt() / CUT;
                    let f = (k2 * self.mass[j] * tp * tp) / d2;
                    ux += dx * f;
                    uy += dy * f;
                }
            }
            self.fx[i] += ux * self.mass[i] * self.rep;
            self.fy[i] += uy * self.mass[i] * self.rep;
        }
    }

    fn springs(&mut self) {
        for e in 0..self.ea.len() {
            let a = self.ea[e];
            let b = self.eb[e];
            let dx = self.x[b] - self.x[a];
            let dy = self.y[b] - self.y[a];
            let mut d = (dx * dx + dy * dy).sqrt();
            if d == 0.0 {
                d = 0.01;
            }
            let f = if d / S < 0.25 { d / S } else { 0.25 };
            self.fx[a] += dx * f;
            self.fy[a] += dy * f;
            self.fx[b] -= dx * f;
            self.fy[b] -= dy * f;
        }
    }

    fn centres(&mut self) {
        for v in [
            &mut self.tx,
            &mut self.ty,
            &mut self.tm,
            &mut self.sx,
            &mut self.sy,
            &mut self.sm,
        ] {
            v.fill(0.0);
        }
        for i in 0..self.n {
            let t = self.top_of[i];
            self.tx[t] += self.x[i];
            self.ty[t] += self.y[i];
            self.tm[t] += 1.0;
            if let Some(u) = self.sub_of[i] {
                self.sx[u] += self.x[i];
                self.sy[u] += self.y[i];
                self.sm[u] += 1.0;
            }
        }
        for t in 0..self.tm.len() {
            if self.tm[t] != 0.0 {
                self.tx[t] /= self.tm[t];
                self.ty[t] /= self.tm[t];
            }
        }
        for u in 0..self.sm.len() {
            if self.sm[u] != 0.0 {
                self.sx[u] /= self.sm[u];
                self.sy[u] /= self.sm[u];
            }
        }
    }

    fn step(&mut self, temp: f64) {
        let temp2 = temp * temp;
        for i in 0..self.n {
            let t = self.top_of[i];
            let mut gx = self.fx[i];
            let mut gy = self.fy[i];
            if self.tm[t] > 1.0 {
                let w = self.top_pull * self.top_w[t] * self.mass[i];
                gx += (self.tx[t] - self.x[i]) * w;
                gy += (self.ty[t] - self.y[i]) * w;
            }
            if let Some(u) = self.sub_of[i] {
                if self.sm[u] > 1.0 {
                    let w = self.sub_pull * self.top_w[t] * self.mass[i];
                    gx += (self.sx[u] - self.x[i]) * w;
                    gy += (self.sy[u] - self.y[i]) * w;
                }
            }
            gx -= self.x[i] * self.ax * self.mass[i];
            gy -= self.y[i] * self.ay * self.mass[i];
            let mx = gx / self.mass[i];
            let my = gy / self.mass[i];
            let step2 = mx * mx + my * my;
            let k = if step2 > temp2 {
                temp / step2.sqrt()
            } else {
                1.0
            };
            self.x[i] += mx * k;
            self.y[i] += my * k;
        }
    }
}

/// Push apart memories closer than their radii plus `gap`.
fn separate(x: &mut [f64], y: &mut [f64], r: &[f64], gap: f64) {
    let n = x.len();
    if n == 0 {
        return;
    }
    let mut max_r = 0.0f64;
    for &v in r {
        if v > max_r {
            max_r = v;
        }
    }
    let cell = max_r * 2.0 + gap;
    let mut cell_of = vec![0usize; n];
    let mut items = vec![0usize; n];
    for _pass in 0..30 {
        let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
        let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for i in 0..n {
            if x[i] < x0 {
                x0 = x[i];
            }
            if x[i] > x1 {
                x1 = x[i];
            }
            if y[i] < y0 {
                y0 = y[i];
            }
            if y[i] > y1 {
                y1 = y[i];
            }
        }
        let gw = (GRID_MAX * 4).min(((x1 - x0) / cell).floor() as usize + 1);
        let gh = (GRID_MAX * 4).min(((y1 - y0) / cell).floor() as usize + 1);
        let mut start = vec![0usize; gw * gh + 1];
        for i in 0..n {
            let c = (gh - 1).min(((y[i] - y0) / cell).floor() as usize) * gw
                + (gw - 1).min(((x[i] - x0) / cell).floor() as usize);
            cell_of[i] = c;
            start[c + 1] += 1;
        }
        for c in 0..gw * gh {
            start[c + 1] += start[c];
        }
        let mut fill = start[..gw * gh].to_vec();
        for i in 0..n {
            items[fill[cell_of[i]]] = i;
            fill[cell_of[i]] += 1;
        }
        let mut moved = 0usize;
        for i in 0..n {
            let ci = (cell_of[i] % gw) as i64;
            let cj = ((cell_of[i] - cell_of[i] % gw) / gw) as i64;
            for oy in -1i64..=1 {
                let rj = cj + oy;
                if rj < 0 || rj >= gh as i64 {
                    continue;
                }
                for ox in -1i64..=1 {
                    let ri = ci + ox;
                    if ri < 0 || ri >= gw as i64 {
                        continue;
                    }
                    let cc = (rj * gw as i64 + ri) as usize;
                    for q in start[cc]..start[cc + 1] {
                        let j = items[q];
                        if j <= i {
                            continue;
                        }
                        let mut dx = x[j] - x[i];
                        let mut dy = y[j] - y[i];
                        let mut d = (dx * dx + dy * dy).sqrt();
                        let min = r[i] + r[j] + gap;
                        if d >= min {
                            continue;
                        }
                        if d < 1e-6 {
                            (dx, dy) = DIRS[(i * 7 + j * 5) % 12];
                            d = 1.0;
                        }
                        let push = (min - d) / 2.0;
                        x[i] -= (dx / d) * push;
                        y[i] -= (dy / d) * push;
                        x[j] += (dx / d) * push;
                        y[j] += (dy / d) * push;
                        moved += 1;
                    }
                }
            }
        }
        if moved == 0 {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Scope names
// ---------------------------------------------------------------------------

fn name_width(len: usize, size: f64) -> f64 {
    len as f64 * size * 0.62 + size * 0.4
}

/// What the name placement reads of a top scope.
struct TopName<'a> {
    key: &'a str,
    key_len: usize,
    slot: Option<u32>,
    cx: f64,
    cy: f64,
    reach: f64,
}

struct Placed {
    names: Vec<ScopeNameOut>,
    boxes: Vec<Rect>,
}

fn place_scope_names(
    tops: &[TopName<'_>],
    x: &[f64],
    y: &[f64],
    r: &[f64],
    size: f64,
    avoid: &[Rect],
) -> Placed {
    let n = x.len();
    let mut out = Vec::new();
    let mut boxes = Vec::new();
    let cell = S.max(size * 2.0);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for i in 0..n {
        grid.entry(((x[i] / cell).floor() as i64, (y[i] / cell).floor() as i64))
            .or_default()
            .push(i);
    }
    let covered = |b: &Rect, stop: usize| -> usize {
        let mut c = 0usize;
        for cx in ((b.x0 - 12.0) / cell).floor() as i64..=((b.x1 + 12.0) / cell).floor() as i64 {
            for cy in ((b.y0 - 12.0) / cell).floor() as i64..=((b.y1 + 12.0) / cell).floor() as i64
            {
                if let Some(list) = grid.get(&(cx, cy)) {
                    for &i in list {
                        let rr = r[i] + 3.0;
                        if x[i] + rr > b.x0
                            && x[i] - rr < b.x1
                            && y[i] + rr > b.y0
                            && y[i] - rr < b.y1
                        {
                            c += 1;
                            if c >= stop {
                                return c;
                            }
                        }
                    }
                }
            }
        }
        c
    };
    let hits = |b: &Rect, o: &Rect| {
        b.x0 < o.x1 + 6.0 && o.x0 - 6.0 < b.x1 && b.y0 < o.y1 + 4.0 && o.y0 - 4.0 < b.y1
    };
    let span = |b: &Rect| {
        let cx0 = ((b.x0 - 6.0) / cell).floor();
        let cx1 = ((b.x1 + 6.0) / cell).floor();
        let cy0 = ((b.y0 - 4.0) / cell).floor();
        let cy1 = ((b.y1 + 4.0) / cell).floor();
        let mut keys = Vec::new();
        let mut cx = cx0;
        while cx <= cx1 {
            let mut cy = cy0;
            while cy <= cy1 {
                keys.push(fkey(cx * 65536.0 + cy));
                cy += 1.0;
            }
            cx += 1.0;
        }
        keys
    };
    let mut taken: HashMap<u64, Vec<Rect>> = HashMap::new();
    let take = |taken: &mut HashMap<u64, Vec<Rect>>, b: Rect| {
        for key in span(&b) {
            taken.entry(key).or_default().push(b);
        }
    };
    for b in avoid {
        take(&mut taken, *b);
    }
    let on_name = |taken: &HashMap<u64, Vec<Rect>>, b: &Rect| {
        span(b).into_iter().any(|key| {
            taken
                .get(&key)
                .is_some_and(|list| list.iter().any(|o| hits(b, o)))
        })
    };
    for t in tops {
        if t.key.is_empty() {
            continue;
        }
        let w = name_width(t.key_len, size);
        let h = size * 1.2;
        let limit = (size * 2.0).max((t.reach * 0.5).min(size * 6.0));
        let step = (size * 0.7).max(limit / 12.0);
        let rings = (limit / step).ceil();
        // (covered, cx, cy, box)
        let mut best: Option<(usize, f64, f64, Rect)> = None;
        let mut ring = 0i64;
        while (ring as f64) <= rings && !best.is_some_and(|b| b.0 == 0) {
            let mut cand: Vec<(i64, i64)> = Vec::new();
            if ring == 0 {
                cand.push((0, 0));
            } else {
                for k in -ring..=ring {
                    cand.push((k, -ring));
                    cand.push((k, ring));
                }
                for k in -ring + 1..=ring - 1 {
                    cand.push((-ring, k));
                    cand.push((ring, k));
                }
                cand.sort_by(|p, q| {
                    (p.0 * p.0 + p.1 * p.1)
                        .cmp(&(q.0 * q.0 + q.1 * q.1))
                        .then(p.1.cmp(&q.1))
                        .then(p.0.cmp(&q.0))
                });
            }
            for (ox, oy) in cand {
                let cx = t.cx + ox as f64 * step;
                let cy = t.cy + oy as f64 * step * 0.6;
                let b = Rect {
                    x0: cx - w / 2.0,
                    y0: cy - h / 2.0,
                    x1: cx + w / 2.0,
                    y1: cy + h / 2.0,
                };
                if on_name(&taken, &b) {
                    continue;
                }
                let c = covered(&b, best.map_or(usize::MAX, |b| b.0));
                if best.is_none_or(|bb| c < bb.0) {
                    best = Some((c, cx, cy, b));
                }
                if c == 0 {
                    break;
                }
            }
            ring += 1;
        }
        let (cx, cy) = best.map_or((t.cx, t.cy), |b| (b.1, b.2));
        let b = best.map_or(
            Rect {
                x0: cx - w / 2.0,
                y0: cy - h / 2.0,
                x1: cx + w / 2.0,
                y1: cy + h / 2.0,
            },
            |b| b.3,
        );
        if best.is_some() {
            boxes.push(b);
            take(&mut taken, b);
        }
        out.push(ScopeNameOut {
            key: t.key.to_string(),
            slot: t.slot,
            x: cx,
            y: cy + size * 0.36,
            size,
            w,
            placed: best.is_some(),
            backed: best.is_some_and(|b| b.0 > 0),
            rect: b,
        });
    }
    Placed { names: out, boxes }
}

/// `Math.sign`, with NaN read as 0 (a NaN product is never `< 0` either).
fn sign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

#[allow(clippy::too_many_arguments)]
fn crosses(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, dx: f64, dy: f64) -> bool {
    let o = |px: f64, py: f64, qx: f64, qy: f64, rx: f64, ry: f64| {
        sign((qx - px) * (ry - py) - (qy - py) * (rx - px))
    };
    o(ax, ay, bx, by, cx, cy) * o(ax, ay, bx, by, dx, dy) < 0.0
        && o(cx, cy, dx, dy, ax, ay) * o(cx, cy, dx, dy, bx, by) < 0.0
}

/// The label nudge's test (`nudgeTest` in the JS; its docs say why).
struct NudgeTest {
    adj: Vec<Vec<usize>>,
    segs: Vec<(usize, usize)>,
    segs_of: Vec<Vec<usize>>,
    grid: HashMap<u64, Vec<usize>>,
    cell: f64,
    stamp: Vec<u32>,
    query: u32,
    work: u64,
    /// Per top scope: centre and whether it is named.
    tops: Vec<(f64, f64, bool)>,
    top_of_node: Vec<usize>,
    base_of: Option<usize>,
    base: usize,
}

impl NudgeTest {
    fn cells_of(&self, ax: f64, ay: f64, bx: f64, by: f64) -> Vec<u64> {
        let cx0 = (ax.min(bx) / self.cell).floor();
        let cx1 = (ax.max(bx) / self.cell).floor();
        let cy0 = (ay.min(by) / self.cell).floor();
        let cy1 = (ay.max(by) / self.cell).floor();
        let mut out = Vec::new();
        let mut cx = cx0;
        while cx <= cx1 {
            let mut cy = cy0;
            while cy <= cy1 {
                out.push(fkey(cx * 65536.0 + cy));
                cy += 1.0;
            }
            cx += 1.0;
        }
        out
    }

    fn file(&mut self, k: usize, pos: &[f64]) {
        let (a, b) = self.segs[k];
        for key in self.cells_of(pos[2 * a], pos[2 * a + 1], pos[2 * b], pos[2 * b + 1]) {
            self.grid.entry(key).or_default().push(k);
        }
    }

    fn count(&mut self, i: usize, x: f64, y: f64, pos: &[f64]) -> usize {
        let mut c = 0;
        for jj in 0..self.adj[i].len() {
            let j = self.adj[i][jj];
            let jx = pos[2 * j];
            let jy = pos[2 * j + 1];
            self.query += 1;
            for key in self.cells_of(x, y, jx, jy) {
                let Some(list) = self.grid.get(&key) else {
                    continue;
                };
                for &k in list {
                    if self.stamp[k] == self.query {
                        continue;
                    }
                    self.stamp[k] = self.query;
                    let (a, b) = self.segs[k];
                    if a == i || b == i || a == j || b == j {
                        continue;
                    }
                    self.work += 1;
                    if crosses(
                        x,
                        y,
                        jx,
                        jy,
                        pos[2 * a],
                        pos[2 * a + 1],
                        pos[2 * b],
                        pos[2 * b + 1],
                    ) {
                        c += 1;
                    }
                }
            }
        }
        c
    }

    fn intrusion(&self, own: usize, x: f64, y: f64) -> f64 {
        let mut other = f64::INFINITY;
        for (t, &(cx, cy, named)) in self.tops.iter().enumerate() {
            if t == own || !named {
                continue;
            }
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy < other {
                other = dx * dx + dy * dy;
            }
        }
        let dx = x - self.tops[own].0;
        let dy = y - self.tops[own].1;
        dx * dx + dy * dy - other
    }
}

impl Nudge for NudgeTest {
    fn accept(&mut self, i: usize, x: f64, y: f64, pos: &[f64]) -> bool {
        if self.work > NUDGE_WORK || self.adj[i].len() > NUDGE_DEGREE {
            return false;
        }
        let own = self.top_of_node[i];
        if self.tops[own].2 {
            let after = self.intrusion(own, x, y);
            if after > 0.0 && after > self.intrusion(own, pos[2 * i], pos[2 * i + 1]) {
                return false;
            }
        }
        if self.adj[i].is_empty() {
            return true;
        }
        if self.base_of != Some(i) {
            self.base_of = Some(i);
            self.base = self.count(i, pos[2 * i], pos[2 * i + 1], pos);
        }
        self.count(i, x, y, pos) <= self.base
    }

    fn moved(&mut self, i: usize, pos: &[f64]) {
        for kk in 0..self.segs_of[i].len() {
            let k = self.segs_of[i][kk];
            self.file(k, pos);
        }
    }
}

// ---------------------------------------------------------------------------
// The layout
// ---------------------------------------------------------------------------

struct Group {
    key: String,
    members: Vec<usize>,
}

struct Top {
    key: String,
    key_len: usize,
    slot: Option<u32>,
    members: Vec<usize>,
    groups: Vec<Group>,
    aff: Option<Vec<f64>>,
    /// (group index × nTop + other top) → relations, in insertion order.
    ext: Vec<(usize, f64)>,
    ext_at: HashMap<usize, usize>,
    local: Vec<f64>,
    rad: f64,
    cx: f64,
    cy: f64,
    reach: f64,
}

impl Top {
    fn ext_add(&mut self, key: usize) {
        match self.ext_at.get(&key) {
            Some(&at) => self.ext[at].1 += 1.0,
            None => {
                self.ext_at.insert(key, self.ext.len());
                self.ext.push((key, 1.0));
            }
        }
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `layout(input)` of scope-force.js: [`layout_core`], then [`finish`] for
/// the input's width.
pub fn layout(input: &ForceInput) -> ForceOutput {
    finish(&layout_core(input), input.width)
}

/// Everything of scope-force.js's `layout` but its step 5b and the bounds:
/// the part that does not depend on the box's width (the module docs of
/// [`super`], "Width"), so one cached core serves every width.
pub fn layout_core(input: &ForceInput) -> ForceCore {
    let nodes = &input.nodes;
    let edges = &input.edges;
    let n = nodes.len();
    let aspect = clamp(
        if input.aspect.is_nan() || input.aspect == 0.0 {
            0.62
        } else {
            input.aspect
        },
        0.3,
        3.0,
    );
    let opt = ForceOptions::default();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| js_cmp(&nodes[a].id, &nodes[b].id));
    let mut rank = vec![0usize; n];
    for (k, &i) in order.iter().enumerate() {
        rank[i] = k;
    }
    let node = |k: usize| &nodes[order[k]];
    let r: Vec<f64> = (0..n)
        .map(|k| if node(k).r != 0.0 { node(k).r } else { 6.0 })
        .collect();

    let valid = |v: i64| v >= 0 && (v as usize) < n;
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for e in edges {
        if e.t >= 0 && e.t != e.s && valid(e.s) && valid(e.t) {
            let a = rank[e.s as usize];
            let b = rank[e.t as usize];
            pairs.push(if a < b { (a, b) } else { (b, a) });
        }
    }
    pairs.sort();
    let ea: Vec<usize> = pairs.iter().map(|p| p.0).collect();
    let eb: Vec<usize> = pairs.iter().map(|p| p.1).collect();
    let mut deg = vec![0.0f64; n];
    for &(a, b) in &pairs {
        deg[a] += 1.0;
        deg[b] += 1.0;
    }
    let mass: Vec<f64> = deg.iter().map(|d| 1.0 + d * 0.5).collect();

    // ---- scopes
    let mut top_at: HashMap<&str, usize> = HashMap::new();
    let mut tops: Vec<Top> = Vec::new();
    let mut top_of_name: Vec<&str> = Vec::with_capacity(n);
    for k in 0..n {
        let top = node(k).top_scope.as_str();
        top_of_name.push(top);
        let at = *top_at.entry(top).or_insert_with(|| {
            tops.push(Top {
                key: top.to_string(),
                key_len: utf16_len(top),
                slot: node(k).slot,
                members: Vec::new(),
                groups: Vec::new(),
                aff: None,
                ext: Vec::new(),
                ext_at: HashMap::new(),
                local: Vec::new(),
                rad: 0.0,
                cx: 0.0,
                cy: 0.0,
                reach: 0.0,
            });
            tops.len() - 1
        });
        tops[at].members.push(k);
    }
    tops.sort_by(|a, b| {
        (a.key.is_empty() as u8)
            .cmp(&(b.key.is_empty() as u8))
            .then(b.members.len().cmp(&a.members.len()))
            .then_with(|| js_cmp(&a.key, &b.key))
    });
    let n_top = tops.len();
    let mut top_of = vec![0usize; n];
    for (ti, t) in tops.iter().enumerate() {
        for &k in &t.members {
            top_of[k] = ti;
        }
    }
    let mut sub_of: Vec<Option<usize>> = vec![None; n];
    let mut n_sub = 0usize;
    for t in tops.iter_mut() {
        let mut at: HashMap<&str, usize> = HashMap::new();
        let mut groups: Vec<Group> = Vec::new();
        for &k in &t.members {
            let full = node(k).scope.as_str();
            let key = if !full.is_empty() && full != t.key {
                full
            } else {
                ""
            };
            let g = *at.entry(key).or_insert_with(|| {
                groups.push(Group {
                    key: key.to_string(),
                    members: Vec::new(),
                });
                groups.len() - 1
            });
            groups[g].members.push(k);
        }
        groups.sort_by(|a, b| {
            b.members
                .len()
                .cmp(&a.members.len())
                .then_with(|| js_cmp(&a.key, &b.key))
        });
        for gr in &groups {
            if gr.key.is_empty() {
                continue;
            }
            for &k in &gr.members {
                sub_of[k] = Some(n_sub);
            }
            n_sub += 1;
        }
        t.groups = groups;
    }
    let mut inside = vec![0.0f64; n_top];
    let mut ends = vec![0.0f64; n_top];
    let mut cross = 0.0f64;
    for e in 0..ea.len() {
        let ta = top_of[ea[e]];
        let tb = top_of[eb[e]];
        ends[ta] += 1.0;
        ends[tb] += 1.0;
        if ta == tb {
            inside[ta] += 2.0;
        } else {
            cross += 1.0;
        }
    }
    let top_w: Vec<f64> = tops
        .iter()
        .enumerate()
        .map(|(ti, t)| {
            let kept = if ends[ti] != 0.0 {
                inside[ti] / ends[ti]
            } else {
                1.0
            };
            (if t.key.is_empty() { 0.4 } else { 1.0 }) * (0.25 + 0.75 * 1f64.min(kept / 0.6))
        })
        .collect();
    let cross_share = if ea.is_empty() {
        0.0
    } else {
        cross / ea.len() as f64
    };

    let mut x = vec![0.0f64; n];
    let mut y = vec![0.0f64; n];
    if n > 0 {
        let mut group_of = vec![0usize; n];
        for t in &tops {
            for (gi, gr) in t.groups.iter().enumerate() {
                for &k in &gr.members {
                    group_of[k] = gi;
                }
            }
        }
        let pad = S * 0.6;
        let mut top_aff = vec![0.0f64; n_top * n_top];
        for e in 0..ea.len() {
            let ta = top_of[ea[e]];
            let tb = top_of[eb[e]];
            if ta != tb {
                top_aff[ta * n_top + tb] += 1.0;
                top_aff[tb * n_top + ta] += 1.0;
            }
        }
        for t in tops.iter_mut() {
            let m = t.groups.len();
            t.aff = (m > 1).then(|| vec![0.0f64; m * m]);
        }
        for e in 0..ea.len() {
            let a = ea[e];
            let b = eb[e];
            let ta = top_of[a];
            let tb = top_of[b];
            if ta == tb {
                let t = &mut tops[ta];
                let m = t.groups.len();
                if let Some(aff) = t.aff.as_mut() {
                    if group_of[a] != group_of[b] {
                        aff[group_of[a] * m + group_of[b]] += 1.0;
                        aff[group_of[b] * m + group_of[a]] += 1.0;
                    }
                }
            } else {
                tops[ta].ext_add(group_of[a] * n_top + tb);
                tops[tb].ext_add(group_of[b] * n_top + ta);
            }
        }
        for t in tops.iter_mut() {
            let m = t.groups.len();
            let rad: Vec<f64> = t
                .groups
                .iter()
                .map(|gr| SEED * (gr.members.len() as f64).sqrt() + S * 0.3)
                .collect();
            t.local = pack(&rad, t.aff.as_deref(), aspect, pad * 0.5);
            let mut cx = 0.0f64;
            let mut cy = 0.0f64;
            let mut wsum = 0.0f64;
            for gi in 0..m {
                let w = t.groups[gi].members.len() as f64;
                cx += t.local[2 * gi] * w;
                cy += t.local[2 * gi + 1] * w;
                wsum += w;
            }
            cx /= wsum;
            cy /= wsum;
            let mut reach = 0.0f64;
            for (gi, rg) in rad.iter().enumerate() {
                t.local[2 * gi] -= cx;
                t.local[2 * gi + 1] -= cy;
                let lx = t.local[2 * gi];
                let ly = t.local[2 * gi + 1];
                let d = (lx * lx + ly * ly).sqrt() + rg;
                if d > reach {
                    reach = d;
                }
            }
            t.rad = reach;
        }
        let top_rad: Vec<f64> = tops.iter().map(|t| t.rad).collect();
        let centres = pack(&top_rad, Some(&top_aff), aspect, pad);
        for (ti, t) in tops.iter_mut().enumerate() {
            let tx = centres[2 * ti];
            let ty = centres[2 * ti + 1];
            let mut best_turn = 0usize;
            if t.groups.len() > 1 && !t.ext.is_empty() {
                let mut ext = t.ext.clone();
                if ext.len() > TURN_MAX {
                    ext.sort_by(|p, q| {
                        q.1.partial_cmp(&p.1)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(p.0.cmp(&q.0))
                    });
                    ext.truncate(TURN_MAX);
                }
                let ne = ext.len();
                let mut elx = vec![0.0f64; ne];
                let mut ely = vec![0.0f64; ne];
                let mut eux = vec![0.0f64; ne];
                let mut euy = vec![0.0f64; ne];
                let mut ew = vec![0.0f64; ne];
                for (q, &(key, w)) in ext.iter().enumerate() {
                    let gi = key / n_top;
                    let u = key - gi * n_top;
                    elx[q] = t.local[2 * gi];
                    ely[q] = t.local[2 * gi + 1];
                    eux[q] = centres[2 * u] - tx;
                    euy[q] = centres[2 * u + 1] - ty;
                    ew[q] = w;
                }
                let mut best = f64::INFINITY;
                for (turn, &(c, sn)) in DIRS.iter().enumerate() {
                    let mut cost = 0.0f64;
                    for e in 0..ne {
                        let dx = elx[e] * c - ely[e] * sn - eux[e];
                        let dy = elx[e] * sn + ely[e] * c - euy[e];
                        cost += ew[e] * (dx * dx + dy * dy).sqrt();
                    }
                    if cost < best - 1e-9 {
                        best = cost;
                        best_turn = turn;
                    }
                }
            }
            let (c, sn) = DIRS[best_turn];
            t.cx = tx;
            t.cy = ty;
            t.reach = t.rad;
            for (gi, gr) in t.groups.iter().enumerate() {
                let lx = t.local[2 * gi];
                let ly = t.local[2 * gi + 1];
                let mut ids = gr.members.clone();
                ids.sort_by(|&a, &b| {
                    deg[b]
                        .partial_cmp(&deg[a])
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.cmp(&b))
                });
                sunflower(
                    &ids,
                    &mut x,
                    &mut y,
                    tx + lx * c - ly * sn,
                    ty + lx * sn + ly * c,
                    SEED,
                );
            }
        }
        // ---- 2. simulation
        let base = if n <= 60 {
            220.0
        } else if n <= 300 {
            150.0
        } else if n <= 1200 {
            110.0
        } else {
            90.0
        };
        let iters = js_round(base * opt.steps * (1.0 + cross_share));
        if n > 1 {
            let temp = opt.temp * (1.0 + 1.2 * cross_share);
            let mut sim = Sim {
                n,
                x: &mut x,
                y: &mut y,
                ea: &ea,
                eb: &eb,
                mass: &mass,
                top_of: &top_of,
                top_w: &top_w,
                sub_of: &sub_of,
                fx: vec![0.0; n],
                fy: vec![0.0; n],
                tx: vec![0.0; n_top],
                ty: vec![0.0; n_top],
                tm: vec![0.0; n_top],
                sx: vec![0.0; n_sub],
                sy: vec![0.0; n_sub],
                sm: vec![0.0; n_sub],
                cell_of: vec![0; n],
                items: vec![0; n],
                start: vec![0; 1],
                fill: vec![0; 1],
                cmass: Vec::new(),
                cmx: Vec::new(),
                cmy: Vec::new(),
                far_x: Vec::new(),
                far_y: Vec::new(),
                gw: 1,
                gh: 1,
                rep: opt.repulsion,
                top_pull: opt.top_pull,
                sub_pull: opt.sub_pull,
                ax: opt.gravity * aspect,
                ay: opt.gravity / aspect,
            };
            let t0 = S * temp;
            let iters_n = iters as usize;
            for it in 0..iters_n {
                sim.fx.fill(0.0);
                sim.fy.fill(0.0);
                sim.bin();
                sim.far_field();
                sim.near_field();
                sim.springs();
                sim.centres();
                sim.step(t0 * (1.0 - it as f64 / iters) + S * 0.02);
            }
        }
    }

    // ---- 3. spread
    let mut spread = 1.0f64;
    if n > 1 {
        let (mut w0, mut w1) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut h0, mut h1) = (f64::INFINITY, f64::NEG_INFINITY);
        for k in 0..n {
            if x[k] < w0 {
                w0 = x[k];
            }
            if x[k] > w1 {
                w1 = x[k];
            }
            if y[k] < h0 {
                h0 = y[k];
            }
            if y[k] > h1 {
                h1 = y[k];
            }
        }
        let wide = (w1 - w0).max((h1 - h0) / aspect).max(S);
        let target = SPREAD_TO * 1f64.max((0.62 / aspect).sqrt());
        spread = 1f64.max(3f64.min(target / wide));
        if spread > 1.0 {
            let sx = (spread * 1.35).min(spread.max(target / S.max(w1 - w0)));
            let sy = spread.min((spread / 1.35).max((target * aspect) / S.max(h1 - h0)));
            for k in 0..n {
                x[k] *= sx;
                y[k] *= sy;
            }
        }
    }
    separate(&mut x, &mut y, &r, 6.0);

    // ---- 4. scope names
    let (mut ex0, mut ex1) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut ey0, mut ey1) = (f64::INFINITY, f64::NEG_INFINITY);
    for k in 0..n {
        ex0 = ex0.min(x[k]);
        ex1 = ex1.max(x[k]);
        ey0 = ey0.min(y[k]);
        ey1 = ey1.max(y[k]);
    }
    let extent = if n > 0 {
        (ex1 - ex0).max((ey1 - ey0) / aspect)
    } else {
        0.0
    };
    let name_size = 120f64.min(15f64.max(extent / 70.0));
    let block = S * 3.0 * spread;
    for t in tops.iter_mut() {
        let mut cells: HashMap<u64, f64> = HashMap::new();
        for &k in &t.members {
            let key = fkey((x[k] / block).floor() * 65536.0 + (y[k] / block).floor());
            *cells.entry(key).or_insert(0.0) += 1.0;
        }
        let mut best_key = 0usize;
        let mut best_count = -1.0f64;
        for &k in &t.members {
            let bx = (x[k] / block).floor();
            let by = (y[k] / block).floor();
            let mut c = 0.0f64;
            for ox in [-1.0, 0.0, 1.0] {
                for oy in [-1.0, 0.0, 1.0] {
                    c += cells
                        .get(&fkey((bx + ox) * 65536.0 + by + oy))
                        .copied()
                        .unwrap_or(0.0);
                }
            }
            if c > best_count {
                best_count = c;
                best_key = k;
            }
        }
        let bx = (x[best_key] / block).floor();
        let by = (y[best_key] / block).floor();
        let mut cx = 0.0f64;
        let mut cy = 0.0f64;
        let mut m = 0.0f64;
        for &k in &t.members {
            if ((x[k] / block).floor() - bx).abs() <= 1.0
                && ((y[k] / block).floor() - by).abs() <= 1.0
            {
                cx += x[k];
                cy += y[k];
                m += 1.0;
            }
        }
        t.cx = cx / m;
        t.cy = cy / m;
        let mut reach = 0.0f64;
        for &k in &t.members {
            let dx = x[k] - t.cx;
            let dy = y[k] - t.cy;
            if dx * dx + dy * dy > reach {
                reach = dx * dx + dy * dy;
            }
        }
        t.reach = reach.sqrt() + S;
    }
    let names_in: Vec<TopName<'_>> = tops
        .iter()
        .map(|t| TopName {
            key: &t.key,
            key_len: t.key_len,
            slot: t.slot,
            cx: t.cx,
            cy: t.cy,
            reach: t.reach,
        })
        .collect();
    let Placed { names, boxes } = place_scope_names(&names_in, &x, &y, &r, name_size, &[]);

    // ---- 5. memory labels, in the caller's node order
    let mut pos = vec![0.0f64; n * 2];
    for i in 0..n {
        pos[2 * i] = x[rank[i]];
        pos[2 * i + 1] = y[rank[i]];
    }
    let nudge = if n <= NUDGE_MAX {
        S * spread.min(1.5)
    } else {
        0.0
    };
    let mut test = if nudge != 0.0 {
        let top_of_node: Vec<usize> = (0..n).map(|i| top_of[rank[i]]).collect();
        let mut adj = vec![Vec::new(); n];
        let mut segs = Vec::new();
        for e in edges {
            if !(valid(e.t) && valid(e.s)) || e.t == e.s {
                continue;
            }
            let (s, t) = (e.s as usize, e.t as usize);
            adj[s].push(t);
            adj[t].push(s);
            segs.push((s, t));
        }
        let mut segs_of = vec![Vec::new(); n];
        for (k, &(a, b)) in segs.iter().enumerate() {
            segs_of[a].push(k);
            segs_of[b].push(k);
        }
        let ns = segs.len();
        let mut t = NudgeTest {
            adj,
            segs,
            segs_of,
            grid: HashMap::new(),
            cell: S * 2.0 * spread,
            stamp: vec![0; ns],
            query: 0,
            work: 0,
            tops: tops
                .iter()
                .map(|t| (t.cx, t.cy, !t.key.is_empty()))
                .collect(),
            top_of_node,
            base_of: None,
            base: 0,
        };
        for k in 0..ns {
            t.file(k, &pos);
        }
        Some(t)
    } else {
        None
    };
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
    let labels = place_labels(
        &label_nodes,
        &mut pos,
        &boxes,
        Options {
            corners: true,
            nudge,
            test: test.as_mut().map(|t| t as &mut dyn Nudge),
        },
    );
    for i in 0..n {
        x[rank[i]] = pos[2 * i];
        y[rank[i]] = pos[2 * i + 1];
    }

    // ---- 6. routes
    let centre_of: HashMap<&str, (f64, f64)> = tops
        .iter()
        .filter(|t| !t.key.is_empty())
        .map(|t| (t.key.as_str(), (t.cx, t.cy)))
        .collect();
    let mut stub_count = vec![0usize; n];
    let stub_len = S * 1.5 * spread;
    let ncell = S * 2.0 * spread;
    let mut near: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for k in 0..n {
        near.entry(((x[k] / ncell).floor() as i64, (y[k] / ncell).floor() as i64))
            .or_default()
            .push(k);
    }
    let away_from = |k: usize| -> (f64, f64) {
        let mut vx = 0.0f64;
        let mut vy = 0.0f64;
        let cx = (x[k] / ncell).floor() as i64;
        let cy = (y[k] / ncell).floor() as i64;
        for ox in -1..=1 {
            for oy in -1..=1 {
                let Some(list) = near.get(&(cx + ox, cy + oy)) else {
                    continue;
                };
                for &j in list {
                    if j == k {
                        continue;
                    }
                    let dx = x[k] - x[j];
                    let dy = y[k] - y[j];
                    let d2 = dx * dx + dy * dy;
                    if d2 > ncell * ncell || d2 < 1e-9 {
                        continue;
                    }
                    vx += dx / d2;
                    vy += dy / d2;
                }
            }
        }
        (vx, vy)
    };
    let mut routes: Vec<[f64; 4]> = Vec::with_capacity(edges.len());
    for e in edges {
        if !valid(e.s) {
            routes.push([0.0; 4]);
            continue;
        }
        let k = rank[e.s as usize];
        if valid(e.t) {
            let t = rank[e.t as usize];
            routes.push([x[k], y[k], x[t], y[t]]);
            continue;
        }
        let far = e
            .far_scope
            .as_deref()
            .filter(|f| !f.is_empty())
            .and_then(|f| centre_of.get(f).map(|c| (f, *c)));
        let (mut vx, mut vy) = match far {
            Some((key, (fx, fy))) if key != top_of_name[k] => (fx - x[k], fy - y[k]),
            _ => away_from(k),
        };
        let mut len = (vx * vx + vy * vy).sqrt();
        if len < 1e-9 {
            (vx, vy) = DIRS[10];
            len = 1.0;
        }
        vx /= len;
        vy /= len;
        let j = stub_count[k] as i64;
        stub_count[k] += 1;
        let turn = ((j + 1) >> 1) * if j % 2 != 0 { 1 } else { -1 };
        let (c, sn) = DIRS[turn.rem_euclid(12) as usize];
        let ux = vx * c - vy * sn;
        let uy = vx * sn + vy * c;
        routes.push([x[k], y[k], x[k] + ux * stub_len, y[k] + uy * stub_len]);
    }

    // ---- soft regions
    let mut region_r = S * 0.9 * spread;
    if n > 2 {
        let mut nn: Vec<f64> = Vec::new();
        for k in 0..n {
            let mut best = f64::INFINITY;
            let cx = (x[k] / ncell).floor() as i64;
            let cy = (y[k] / ncell).floor() as i64;
            for ox in -1..=1 {
                for oy in -1..=1 {
                    let Some(list) = near.get(&(cx + ox, cy + oy)) else {
                        continue;
                    };
                    for &j in list {
                        if j == k {
                            continue;
                        }
                        let dx = x[k] - x[j];
                        let dy = y[k] - y[j];
                        let d2 = dx * dx + dy * dy;
                        if d2 < best {
                            best = d2;
                        }
                    }
                }
            }
            if best < f64::INFINITY {
                nn.push(best.sqrt());
            }
        }
        if !nn.is_empty() {
            nn.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let f = 0.5 + 0.5 * 1f64.min(n as f64 / 600.0);
            region_r = (S * 1.2 * spread).min((S * 0.5).max(nn[nn.len() >> 1] * f));
        }
    }

    // ---- bounds
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut grow = |gx: f64, gy: f64| {
        if gx < min_x {
            min_x = gx;
        }
        if gx > max_x {
            max_x = gx;
        }
        if gy < min_y {
            min_y = gy;
        }
        if gy > max_y {
            max_y = gy;
        }
    };
    for k in 0..n {
        let rr = (r[k] + 6.0).max(region_r);
        grow(x[k] - rr, y[k] - rr);
        grow(x[k] + rr, y[k] + rr);
    }
    for rt in &routes {
        grow(rt[0], rt[1]);
        grow(rt[2], rt[3]);
    }
    for l in &labels {
        if l.placed {
            grow(l.rect.x0, l.rect.y0);
            grow(l.rect.x1, l.rect.y1);
        }
    }
    ForceCore {
        pos,
        routes,
        labels,
        region_r,
        x,
        y,
        r,
        tops: tops
            .iter()
            .map(|t| CoreTop {
                key: t.key.clone(),
                key_len: t.key_len,
                slot: t.slot,
                cx: t.cx,
                cy: t.cy,
                reach: t.reach,
            })
            .collect(),
        names,
        name_size,
        extent,
        min: [min_x, min_y],
        max: [max_x, max_y],
    }
}

/// scope-force.js's step 5b and bounds over a [`ForceCore`]: the drawn scope
/// names, at least 12 px on screen at Fit in a box `width` wide, placed
/// again at that size clear of the placed labels (each keeping its step-4
/// spot when no new one is free), and the bounds they grow.
pub fn finish(core: &ForceCore, width: Option<f64>) -> ForceOutput {
    let n = core.x.len();
    let box_w = 200f64.max(match width {
        Some(w) if w != 0.0 && !w.is_nan() => w,
        _ => 900.0,
    });
    let draw_size = 120f64.min(core.name_size.max((12.0 * (core.extent + 200.0)) / box_w));
    let mut shown = core.names.clone();
    if n > 0 && draw_size > core.name_size * 1.01 {
        let names_in: Vec<TopName<'_>> = core
            .tops
            .iter()
            .map(|t| TopName {
                key: &t.key,
                key_len: t.key_len,
                slot: t.slot,
                cx: t.cx,
                cy: t.cy,
                reach: t.reach,
            })
            .collect();
        let avoid: Vec<Rect> = core
            .labels
            .iter()
            .filter(|l| l.placed)
            .map(|l| l.rect)
            .collect();
        let big = place_scope_names(&names_in, &core.x, &core.y, &core.r, draw_size, &avoid).names;
        shown = big
            .into_iter()
            .zip(core.names.iter())
            .map(|(nm, old)| if nm.placed { nm } else { old.clone() })
            .collect();
    }
    let [mut min_x, mut min_y] = core.min;
    let [mut max_x, mut max_y] = core.max;
    let mut grow = |gx: f64, gy: f64| {
        if gx < min_x {
            min_x = gx;
        }
        if gx > max_x {
            max_x = gx;
        }
        if gy < min_y {
            min_y = gy;
        }
        if gy > max_y {
            max_y = gy;
        }
    };
    for nm in &shown {
        if nm.placed {
            grow(nm.rect.x0, nm.rect.y0);
            grow(nm.rect.x1, nm.rect.y1);
        }
    }
    if n == 0 {
        min_x = 0.0;
        min_y = 0.0;
        max_x = S * 2.0;
        max_y = S * 2.0;
    }
    let pad_b = 16.0;
    ForceOutput {
        pos: core.pos.clone(),
        routes: core.routes.clone(),
        labels: core.labels.clone(),
        bounds: [
            min_x - pad_b,
            min_y - pad_b,
            max_x - min_x + pad_b * 2.0,
            max_y - min_y + pad_b * 2.0,
        ],
        scopes: shown,
        region_r: core.region_r,
    }
}
