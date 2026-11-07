//! Collision-free memory labels: the port of `web/js/graph/labels.js`
//! (`placeLabels`), operation for operation, so a label lands on the same
//! spot (and a nudged memory on the same point) as in the page's worker.
//!
//! Most important memory first (degree, then reads, then id); each label
//! takes the first free spot of below / above / right / left (then, with
//! `corners`, the four corners, and with `nudge` a small move of the memory)
//! that hits no memory, no obstacle and no label already placed. One with no
//! free spot comes back at its first spot with `placed: false`.

use std::collections::HashMap;

use super::{js_cmp, Rect};

/// Label metrics in layout units (CSS: `.graph .lbl`, 11.5px, weight 600).
pub const LABEL_H: f64 = 14.0;
const CHAR_W: f64 = 6.6;
/// The grid the hit tests bucket boxes into.
const CELL: f64 = 120.0;

/// `labelWidth(text)` for a label of `len` UTF-16 code units.
pub fn label_width(len: usize) -> f64 {
    (len as f64 * CHAR_W).ceil() + 4.0
}

/// What `placeLabels` reads of a node.
#[derive(Debug, Clone)]
pub struct LabelNode<'a> {
    pub id: &'a str,
    pub r: f64,
    /// The label's length in UTF-16 code units (0: no label).
    pub label_len: usize,
    pub degree: f64,
    pub weight: f64,
}

/// One placed (or fallback) label. `spot` is the index into the spot list
/// (0–3 the sides, 4–7 the corners), from which the page can rebuild `x`,
/// `y`, `anchor` and `box` given the memory's position, radius and width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Label {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub anchor: Anchor,
    pub placed: bool,
    pub spot: u8,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Middle,
    Start,
    End,
}

impl Anchor {
    /// Every anchor, in a fixed order (the layout cache's byte for one).
    pub const ALL: [Anchor; 3] = [Anchor::Middle, Anchor::Start, Anchor::End];

    pub fn as_str(self) -> &'static str {
        match self {
            Anchor::Middle => "middle",
            Anchor::Start => "start",
            Anchor::End => "end",
        }
    }
}

/// The layout's own test for a nudge (labels.js `accept` / `moved`).
pub trait Nudge {
    fn accept(&mut self, i: usize, x: f64, y: f64, pos: &[f64]) -> bool;
    fn moved(&mut self, i: usize, pos: &[f64]);
}

pub struct Options<'a> {
    pub corners: bool,
    /// 0: never move a memory.
    pub nudge: f64,
    pub test: Option<&'a mut dyn Nudge>,
}

struct Grid {
    cells: HashMap<(i64, i64), Vec<usize>>,
    boxes: Vec<(Rect, bool)>,
}

impl Grid {
    fn add(&mut self, b: Rect) -> usize {
        let k = self.boxes.len();
        self.boxes.push((b, false));
        for cx in (b.x0 / CELL).floor() as i64..=(b.x1 / CELL).floor() as i64 {
            for cy in (b.y0 / CELL).floor() as i64..=(b.y1 / CELL).floor() as i64 {
                self.cells.entry((cx, cy)).or_default().push(k);
            }
        }
        k
    }

    fn hits(&self, b: &Rect) -> bool {
        for cx in (b.x0 / CELL).floor() as i64..=(b.x1 / CELL).floor() as i64 {
            for cy in (b.y0 / CELL).floor() as i64..=(b.y1 / CELL).floor() as i64 {
                if let Some(list) = self.cells.get(&(cx, cy)) {
                    for &k in list {
                        let (o, dead) = &self.boxes[k];
                        if !dead && b.x0 < o.x1 && o.x0 < b.x1 && b.y0 < o.y1 && o.y0 < b.y1 {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

#[derive(Clone, Copy)]
struct Spot {
    x: f64,
    y: f64,
    anchor: Anchor,
    b: Rect,
}

fn spots_at(x: f64, y: f64, r: f64, w: f64, corners: bool) -> Vec<Spot> {
    let rect = |x0, y0, x1, y1| Rect { x0, y0, x1, y1 };
    let mut spots = vec![
        Spot {
            x,
            y: y + r + LABEL_H - 2.0,
            anchor: Anchor::Middle,
            b: rect(x - w / 2.0, y + r, x + w / 2.0, y + r + LABEL_H),
        },
        Spot {
            x,
            y: y - r - 4.0,
            anchor: Anchor::Middle,
            b: rect(x - w / 2.0, y - r - LABEL_H, x + w / 2.0, y - r),
        },
        Spot {
            x: x + r + 2.0,
            y: y + 4.0,
            anchor: Anchor::Start,
            b: rect(x + r, y - LABEL_H / 2.0, x + r + 2.0 + w, y + LABEL_H / 2.0),
        },
        Spot {
            x: x - r - 2.0,
            y: y + 4.0,
            anchor: Anchor::End,
            b: rect(x - r - 2.0 - w, y - LABEL_H / 2.0, x - r, y + LABEL_H / 2.0),
        },
    ];
    if corners {
        let c = r * 0.5;
        spots.extend([
            Spot {
                x: x + c,
                y: y + r + LABEL_H - 2.0,
                anchor: Anchor::Start,
                b: rect(x + c, y + r, x + c + w, y + r + LABEL_H),
            },
            Spot {
                x: x - c,
                y: y + r + LABEL_H - 2.0,
                anchor: Anchor::End,
                b: rect(x - c - w, y + r, x - c, y + r + LABEL_H),
            },
            Spot {
                x: x + c,
                y: y - r - 4.0,
                anchor: Anchor::Start,
                b: rect(x + c, y - r - LABEL_H, x + c + w, y - r),
            },
            Spot {
                x: x - c,
                y: y - r - 4.0,
                anchor: Anchor::End,
                b: rect(x - c - w, y - r - LABEL_H, x - c, y - r),
            },
        ]);
    }
    spots
}

/// Nudge directions and lengths (a fraction of `nudge`), nearest first.
fn nudges() -> [(f64, f64); 16] {
    // The same double as the JS literal 0.7071067811865476.
    let d = std::f64::consts::FRAC_1_SQRT_2;
    let unit = [
        (0.0, 1.0),
        (0.0, -1.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (d, d),
        (-d, d),
        (d, -d),
        (-d, -d),
    ];
    let mut out = [(0.0, 0.0); 16];
    let mut k = 0;
    for f in [0.5, 1.0] {
        for (ux, uy) in unit {
            out[k] = (ux * f, uy * f);
            k += 1;
        }
    }
    out
}

/// `placeLabels(nodes, pos, obstacles, opts)`; `pos` is updated in place
/// for every nudged memory.
pub fn place_labels(
    nodes: &[LabelNode<'_>],
    pos: &mut [f64],
    obstacles: &[Rect],
    mut opts: Options<'_>,
) -> Vec<Label> {
    let n = nodes.len();
    let mut grid = Grid {
        cells: HashMap::new(),
        boxes: Vec::with_capacity(n * 2 + obstacles.len()),
    };
    let r_of = |i: usize| if nodes[i].r != 0.0 { nodes[i].r } else { 6.0 };
    // Every memory is an obstacle first, then the caller's.
    let mut discs = vec![0usize; n];
    for (i, disc) in discs.iter_mut().enumerate() {
        let r = r_of(i) + 3.0;
        let x = pos[2 * i];
        let y = pos[2 * i + 1];
        *disc = grid.add(Rect {
            x0: x - r,
            y0: y - r,
            x1: x + r,
            y1: y + r,
        });
    }
    for b in obstacles {
        grid.add(*b);
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (na, nb) = (&nodes[a], &nodes[b]);
        (nb.degree - na.degree)
            .partial_cmp(&0.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                (nb.weight - na.weight)
                    .partial_cmp(&0.0)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then_with(|| js_cmp(na.id, nb.id))
    });
    let nudges = nudges();
    let mut out: Vec<Option<Label>> = vec![None; n];
    for i in order {
        let has_text = nodes[i].label_len > 0;
        let w = label_width(nodes[i].label_len);
        let r = r_of(i) + 4.0;
        let x = pos[2 * i];
        let y = pos[2 * i + 1];
        let spots = spots_at(x, y, r, w, opts.corners);
        let mut free: Option<(usize, Spot)> = None;
        if has_text {
            if let Some(k) = spots.iter().position(|sp| !grid.hits(&sp.b)) {
                free = Some((k, spots[k]));
            }
        }
        if free.is_none() && has_text && opts.nudge != 0.0 {
            // Move the memory a little to where one of its spots is free: its
            // own disc stops counting, the moved disc must hit nothing either.
            grid.boxes[discs[i]].1 = true;
            let rd = r_of(i) + 3.0;
            for (ux, uy) in nudges {
                let nx = x + ux * opts.nudge;
                let ny = y + uy * opts.nudge;
                let disc = Rect {
                    x0: nx - rd,
                    y0: ny - rd,
                    x1: nx + rd,
                    y1: ny + rd,
                };
                if grid.hits(&disc) {
                    continue;
                }
                let moved_spots = spots_at(nx, ny, r, w, opts.corners);
                free = moved_spots
                    .iter()
                    .position(|sp| !grid.hits(&sp.b))
                    .map(|k| (k, moved_spots[k]));
                if free.is_some() {
                    if let Some(test) = opts.test.as_deref_mut() {
                        if !test.accept(i, nx, ny, pos) {
                            free = None;
                        }
                    }
                }
                if free.is_some() {
                    pos[2 * i] = nx;
                    pos[2 * i + 1] = ny;
                    discs[i] = grid.add(disc);
                    if let Some(test) = opts.test.as_deref_mut() {
                        test.moved(i, pos);
                    }
                    break;
                }
            }
            if free.is_none() {
                grid.boxes[discs[i]].1 = false;
            }
        }
        let placed = free.is_some();
        let (spot, sp) = match free {
            Some(f) => f,
            None => (0, spots[0]),
        };
        if placed {
            grid.add(sp.b);
        }
        out[i] = Some(Label {
            x: sp.x,
            y: sp.y,
            w,
            anchor: sp.anchor,
            placed,
            spot: spot as u8,
            rect: sp.b,
        });
    }
    out.into_iter()
        .map(|l| l.expect("every node labelled"))
        .collect()
}
