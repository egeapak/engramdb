//! Server-side graph layouts: the one place the dashboard lays out a graph.
//! `/graph?layout=…` always carries the layout that fits its response, and
//! the page draws it as sent: it has no layout code of its own (only the
//! focus-time relabel of a few lit labels, `graph/labels.js`).
//!
//! **What.** [`scope_force`] (Plain's layout, the small-store scope filter
//! and every opened scope, by sub-scope), [`grid`] (the Grid style's seeded
//! layout), both with [`labels`] (collision-free memory labels), and
//! [`overview`] (the scope overview: one disc per top scope). They began as
//! ports of the page's JS layouts and kept their
//! arithmetic rules (IEEE doubles only, `+ − × ÷ sqrt`, id-ordered loops,
//! maps in insertion order, [`js_round`], UTF-16 string order and lengths):
//! those rules are what make a layout deterministic, the same bytes for the
//! same input on every machine, which the tests check. There is no JS copy
//! to match any more.
//!
//! **The input** is built by `graph.rs` from the very response it sends
//! ([`build_force_input`], [`build_overview_input`]): node order, the group
//! key (`top_scope`, or the sub-scope, [`sub_key`], for an opened scope),
//! radius from reads ([`radius`], the size the page draws a memory at), the
//! label's length ([`short_label_len`], the page's `shortLabel` text),
//! edges with stub ends at `-1`. The box's `aspect` (a multiple of 0.05)
//! and `width` (a multiple of 40 px) come from the page (`layout_aspect=`,
//! `layout_width=`); anything else is a 400, so a client cannot mint
//! unbounded cache keys.
//!
//! **Width.** scope-force's positions, labels and routes do not depend on
//! the width; only the drawn scope names and the bounds they grow do. So
//! the cache holds the width-free [`ForceCore`] under a digest without the
//! width, and each response runs [`scope_force::finish`] for its own width.
//! The overview's whole geometry depends on the width, and it is small: it
//! is cached per width, as encoded JSON.
//!
//! **Encoding** (`layout.v` = [`LAYOUT_VERSION`]). The layout rides in the
//! same JSON document as the nodes and edges it lays out, so it is keyed by
//! their order there and needs no check that it fits:
//! - scope-force: `name`, `groups` (`top` | `sub`), `key` (the cache
//!   digest), `aspect`, `width`; `pos` (two doubles per entry of `nodes`),
//!   `routes` (four per entry of `edges`: its two ends, a stub's far end
//!   included), `label_xy` (two per node) and `label_flags` (one byte per
//!   node: the anchor, 0 middle / 1 start / 2 end, plus `0x80` when the label
//!   found a free spot), each little-endian `f64` / bytes in base64;
//!   `group_keys` and `group` (per node, an index into `group_keys`: the
//!   region it was laid out in), `bounds`, `scopes` (the drawn names),
//!   `region_r`, and for an opened scope `dirs` (far scope → the direction
//!   it lies in on the scope overview, for the page's tags).
//! - grid: `name`, `key`, `aspect`; `pos`, `label_xy`, `label_flags` as
//!   for scope-force; `routes` every edge's points in edge order (two
//!   doubles each) with `route_points` (one byte per edge: how many it has);
//!   `chains` (per top scope: `key`, `slot`, `pts`, the scope's path through
//!   its memories) and `bounds` as plain JSON.
//! - scope-overview: plain JSON: `ids` (disc `i`'s scope name), `pairs`
//!   (band `k`'s two scope names), `pos`, `r`, `routes`, `labels`, `bounds`.
//!
//! The server checks every number is finite before sending one; a layout
//! that is not is an error, never drawn.
//!
//! **Cache** ([`cache`]): in memory, and on disk under the OS cache dir —
//! the dashboard's one declared write outside a store (see `cache.rs`).
//! Every digest is salted with [`LAYOUT_VERSION`] and the crate version, so
//! a new binary never serves an old binary's layout.

pub mod cache;
pub mod grid;
pub mod labels;
pub mod overview;
pub mod scope_force;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The encoding and algorithm version: part of every digest, so a change to
/// either (bump it) never serves an old cached layout. Bump it for any
/// change to a layout's output or to the wire format.
pub const LAYOUT_VERSION: u32 = 4;

/// An axis-aligned box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// JS string order (`a < b`): UTF-16 code units.
pub fn js_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `Math.round`: halves round up (toward +∞).
pub fn js_round(v: f64) -> f64 {
    let f = v.floor();
    if v - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// A computed number as a map key, as a JS `Map` keys it (SameValueZero:
/// `-0` and `0` are one key).
pub(crate) fn fkey(v: f64) -> u64 {
    (v + 0.0).to_bits()
}

/// JS `\s` (and `String.prototype.trim`'s set).
fn is_js_space(c: u16) -> bool {
    matches!(
        c,
        0x09..=0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

fn trim(s: &[u16]) -> &[u16] {
    let start = s.iter().position(|&c| !is_js_space(c)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|&c| !is_js_space(c))
        .map_or(start, |e| e + 1);
    &s[start..end.max(start)]
}

/// The UTF-16 length of the page's label text (`graph/input.js::shortLabel`:
/// 24 units at most plus an ellipsis): all a layout reads of a label is its
/// width, and this keeps it the width of the text the page draws.
pub fn short_label_len(summary: &str) -> usize {
    short_label(summary).len()
}

fn short_label(summary: &str) -> Vec<u16> {
    const MAX: usize = 24;
    let all: Vec<u16> = summary.encode_utf16().collect();
    let mut t: Vec<u16> = trim(&all).to_vec();
    // t.search(/[:;—–(]|\.\s|,\s| - /)
    let cut = (0..t.len()).find(|&i| {
        let c = t[i];
        let next_space = t.get(i + 1).is_some_and(|&n| is_js_space(n));
        matches!(c, 0x3a | 0x3b | 0x2014 | 0x2013 | 0x28)
            || ((c == 0x2e || c == 0x2c) && next_space)
            || (c == 0x20 && t.get(i + 1) == Some(&0x2d) && t.get(i + 2) == Some(&0x20))
    });
    if let Some(cut) = cut {
        if cut > 8 {
            t = trim(&t[..cut]).to_vec();
        }
    }
    if t.len() <= MAX {
        return t;
    }
    // t.slice(0, max).split(/\s+/), drop the last word, join with ' '.
    let head = &t[..MAX];
    let mut words: Vec<&[u16]> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < head.len() {
        if is_js_space(head[i]) {
            words.push(&head[start..i]);
            while i < head.len() && is_js_space(head[i]) {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }
    words.push(&head[start..]);
    if words.len() > 1 {
        words.pop();
    }
    let mut out: Vec<u16> = Vec::new();
    for (k, w) in words.iter().enumerate() {
        if k > 0 {
            out.push(0x20);
        }
        out.extend_from_slice(w);
    }
    // .replace(/[,.:;\s]+$/, '') + '…'
    while out
        .last()
        .is_some_and(|&c| matches!(c, 0x2c | 0x2e | 0x3a | 0x3b) || is_js_space(c))
    {
        out.pop();
    }
    out.push(0x2026);
    out
}

/// A memory's radius: 5, growing with reads up to 11 (the size the page
/// draws it at, `graph/radius.js::radius`).
pub fn radius(reads: u64) -> f64 {
    5.0 + 6f64.min((reads as f64).sqrt() * 0.55)
}

/// An opened scope's region key: the first two segments of the memory's
/// scope, else its top scope.
pub fn sub_key(scope: &str, top_scope: &str) -> String {
    let mut parts = scope.split('.');
    match (parts.next(), parts.next()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => format!("{a}.{b}"),
        _ => top_scope.to_string(),
    }
}

/// `toLocaleString('en-US')` of a count: its UTF-16 length (digits plus a
/// comma per three).
pub fn fmt_len(n: usize) -> usize {
    let digits = n.to_string().len();
    digits + (digits - 1) / 3
}

/// One memory of scope-force's input (the label is only its length: all
/// the layout reads of it).
#[derive(Debug, Clone, Serialize)]
pub struct ForceNode {
    pub id: String,
    pub top_scope: String,
    pub slot: Option<u32>,
    pub scope: String,
    pub degree: f64,
    pub r: f64,
    pub label_len: usize,
    pub weight: f64,
}

/// One relation: node indices, `t < 0` a stub off `s`.
#[derive(Debug, Clone, Serialize)]
pub struct ForceEdge {
    pub s: i64,
    pub t: i64,
    pub far_scope: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForceInput {
    pub nodes: Vec<ForceNode>,
    pub edges: Vec<ForceEdge>,
    pub aspect: f64,
    pub width: Option<f64>,
}

/// One drawn scope name (scope-force's `scopes`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeNameOut {
    pub key: String,
    pub slot: Option<u32>,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub w: f64,
    pub placed: bool,
    pub backed: bool,
    pub rect: Rect,
}

/// scope-force's output, before encoding. `routes` are `[x0, y0, x1, y1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ForceOutput {
    pub pos: Vec<f64>,
    pub routes: Vec<[f64; 4]>,
    pub labels: Vec<labels::Label>,
    /// x, y, w, h.
    pub bounds: [f64; 4],
    pub scopes: Vec<ScopeNameOut>,
    pub region_r: f64,
}

/// A top scope as the name placement reads it (scope-force's `tops`).
#[derive(Debug, Clone, PartialEq)]
pub struct CoreTop {
    pub key: String,
    pub key_len: usize,
    pub slot: Option<u32>,
    pub cx: f64,
    pub cy: f64,
    pub reach: f64,
}

/// scope-force's width-free result ([`scope_force::layout_core`]): the
/// output but the drawn names and bounds, plus what step 5b reads to place
/// them for a width ([`scope_force::finish`]). `x`, `y`, `r` are in the
/// layout's id order; `min` / `max` bound everything but the names.
#[derive(Debug, Clone, PartialEq)]
pub struct ForceCore {
    pub pos: Vec<f64>,
    pub routes: Vec<[f64; 4]>,
    pub labels: Vec<labels::Label>,
    pub region_r: f64,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub r: Vec<f64>,
    pub tops: Vec<CoreTop>,
    pub names: Vec<ScopeNameOut>,
    pub name_size: f64,
    pub extent: f64,
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// A little-endian byte writer for [`Cached::to_bytes`].
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64s(&mut self, v: &[f64]) {
        self.u64(v.len() as u64);
        for &x in v {
            self.f64(x);
        }
    }
    fn str(&mut self, v: &str) {
        self.u64(v.len() as u64);
        self.0.extend_from_slice(v.as_bytes());
    }
    fn slot(&mut self, v: Option<u32>) {
        self.u64(v.map_or(u64::MAX, u64::from));
    }
    fn rect(&mut self, r: &Rect) {
        for v in [r.x0, r.y0, r.x1, r.y1] {
            self.f64(v);
        }
    }
}

/// The reader: every read is bounds-checked, so a truncated or corrupt file
/// is `None` (a cache miss), never a panic.
struct In<'a>(&'a [u8]);

impl In<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn len(&mut self, unit: usize) -> Option<usize> {
        let n = usize::try_from(self.u64()?).ok()?;
        // Never allocate more than the bytes left could hold.
        (n.checked_mul(unit)? <= self.0.len()).then_some(n)
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f64s(&mut self) -> Option<Vec<f64>> {
        let n = self.len(8)?;
        (0..n).map(|_| self.f64()).collect()
    }
    fn str(&mut self) -> Option<String> {
        let n = self.len(1)?;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn slot(&mut self) -> Option<Option<u32>> {
        match self.u64()? {
            u64::MAX => Some(None),
            v => Some(Some(u32::try_from(v).ok()?)),
        }
    }
    fn rect(&mut self) -> Option<Rect> {
        Some(Rect {
            x0: self.f64()?,
            y0: self.f64()?,
            x1: self.f64()?,
            y1: self.f64()?,
        })
    }
    fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

fn anchor_byte(a: labels::Anchor) -> u8 {
    labels::Anchor::ALL
        .iter()
        .position(|&b| b == a)
        .unwrap_or(0) as u8
}

impl ForceCore {
    fn write(&self, o: &mut Out) {
        o.f64s(&self.pos);
        o.u64(self.routes.len() as u64);
        for r in &self.routes {
            for &v in r {
                o.f64(v);
            }
        }
        o.u64(self.labels.len() as u64);
        for l in &self.labels {
            o.f64(l.x);
            o.f64(l.y);
            o.f64(l.w);
            o.u8(anchor_byte(l.anchor));
            o.u8(u8::from(l.placed));
            o.u8(l.spot);
            o.rect(&l.rect);
        }
        o.f64(self.region_r);
        o.f64s(&self.x);
        o.f64s(&self.y);
        o.f64s(&self.r);
        o.u64(self.tops.len() as u64);
        for t in &self.tops {
            o.str(&t.key);
            o.u64(t.key_len as u64);
            o.slot(t.slot);
            o.f64(t.cx);
            o.f64(t.cy);
            o.f64(t.reach);
        }
        o.u64(self.names.len() as u64);
        for nm in &self.names {
            o.str(&nm.key);
            o.slot(nm.slot);
            for v in [nm.x, nm.y, nm.size, nm.w] {
                o.f64(v);
            }
            o.u8(u8::from(nm.placed));
            o.u8(u8::from(nm.backed));
            o.rect(&nm.rect);
        }
        for v in [
            self.name_size,
            self.extent,
            self.min[0],
            self.min[1],
            self.max[0],
            self.max[1],
        ] {
            o.f64(v);
        }
    }

    fn read(i: &mut In<'_>) -> Option<Self> {
        let pos = i.f64s()?;
        let n_routes = i.len(32)?;
        let routes = (0..n_routes)
            .map(|_| Some([i.f64()?, i.f64()?, i.f64()?, i.f64()?]))
            .collect::<Option<Vec<_>>>()?;
        let n_labels = i.len(59)?;
        let labels = (0..n_labels)
            .map(|_| {
                Some(labels::Label {
                    x: i.f64()?,
                    y: i.f64()?,
                    w: i.f64()?,
                    anchor: *labels::Anchor::ALL.get(usize::from(i.u8()?))?,
                    placed: i.bool()?,
                    spot: i.u8()?,
                    rect: i.rect()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let region_r = i.f64()?;
        let (x, y, r) = (i.f64s()?, i.f64s()?, i.f64s()?);
        let n_tops = i.len(40)?;
        let tops = (0..n_tops)
            .map(|_| {
                Some(CoreTop {
                    key: i.str()?,
                    key_len: usize::try_from(i.u64()?).ok()?,
                    slot: i.slot()?,
                    cx: i.f64()?,
                    cy: i.f64()?,
                    reach: i.f64()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let n_names = i.len(82)?;
        let names = (0..n_names)
            .map(|_| {
                Some(ScopeNameOut {
                    key: i.str()?,
                    slot: i.slot()?,
                    x: i.f64()?,
                    y: i.f64()?,
                    size: i.f64()?,
                    w: i.f64()?,
                    placed: i.bool()?,
                    backed: i.bool()?,
                    rect: i.rect()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let core = ForceCore {
            pos,
            routes,
            labels,
            region_r,
            x,
            y,
            r,
            tops,
            names,
            name_size: i.f64()?,
            extent: i.f64()?,
            min: [i.f64()?, i.f64()?],
            max: [i.f64()?, i.f64()?],
        };
        let n = core.x.len();
        let consistent = core.pos.len() == 2 * n
            && core.y.len() == n
            && core.r.len() == n
            && core.labels.len() == n
            && core.names.len() == core.tops.iter().filter(|t| !t.key.is_empty()).count();
        consistent.then_some(core)
    }
}

/// A computed layout as the cache holds it: scope-force's width-free core,
/// or the overview's encoded JSON text (kept as text rather than a
/// `serde_json::Value` because serde_json, built without `float_roundtrip`,
/// does not parse every double back to the same bits).
#[derive(Debug, Clone, PartialEq)]
pub enum Cached {
    Force(Box<ForceCore>),
    Overview(String),
    /// Grid's encoded JSON text (width-free, so cached as sent).
    Grid(String),
}

impl Cached {
    /// The bytes a cache file holds after its header (`cache.rs`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut o = Out::default();
        match self {
            Cached::Force(core) => {
                o.u8(0);
                core.write(&mut o);
            }
            Cached::Overview(text) => {
                o.u8(1);
                o.str(text);
            }
            Cached::Grid(text) => {
                o.u8(2);
                o.str(text);
            }
        }
        o.0
    }

    /// The inverse of [`Cached::to_bytes`]; `None` for anything else.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut i = In(bytes);
        let v = match i.u8()? {
            0 => Cached::Force(Box::new(ForceCore::read(&mut i)?)),
            1 => Cached::Overview(i.str()?),
            2 => Cached::Grid(i.str()?),
            _ => return None,
        };
        i.0.is_empty().then_some(v)
    }
}

/// What a memory's layout input is built from: its fields in the `/graph`
/// response (`graph.rs`'s node, or a test fixture's).
#[derive(Debug, Clone, Copy)]
pub struct NodeFacts<'a> {
    pub id: &'a str,
    pub top_scope: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub summary: &'a str,
    /// Reads of every signal (`graph/radius.js::reads`).
    pub reads: u64,
    pub degree: usize,
    pub slot: Option<u32>,
}

/// A relation's ends in the response.
#[derive(Debug, Clone, Copy)]
pub struct EdgeFacts<'a> {
    pub src: &'a str,
    pub dst: &'a str,
    /// The far end is in another store (`dst_store` set).
    pub cross_store: bool,
}

/// How an opened scope groups its memories: by top scope or sub-scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Groups {
    Top,
    Sub,
}

impl Groups {
    pub fn as_str(self) -> &'static str {
        match self {
            Groups::Top => "top",
            Groups::Sub => "sub",
        }
    }
}

/// scope-force's input for a response's nodes and edges (in their order:
/// the encoded layout is parallel to them).
pub fn build_force_input(
    nodes: &[NodeFacts<'_>],
    edges: &[EdgeFacts<'_>],
    groups: Groups,
    aspect: f64,
    width: f64,
) -> ForceInput {
    let by_id: std::collections::HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();
    let top = |n: &NodeFacts<'_>| n.top_scope.unwrap_or_default().to_string();
    ForceInput {
        nodes: nodes
            .iter()
            .map(|n| ForceNode {
                id: n.id.to_string(),
                top_scope: match groups {
                    Groups::Sub => sub_key(n.scope.unwrap_or(""), n.top_scope.unwrap_or("")),
                    Groups::Top => top(n),
                },
                slot: n.slot,
                scope: n.scope.unwrap_or_default().to_string(),
                degree: n.degree as f64,
                r: radius(n.reads),
                label_len: short_label_len(n.summary),
                weight: n.reads as f64,
            })
            .collect(),
        edges: edges
            .iter()
            .map(|e| {
                let t = if e.cross_store {
                    None
                } else {
                    by_id.get(e.dst).copied()
                };
                ForceEdge {
                    s: by_id.get(e.src).map_or(-1, |&s| s as i64),
                    t: t.map_or(-1, |t| t as i64),
                    far_scope: t.map(|t| top(&nodes[t])),
                }
            })
            .collect(),
        aspect,
        width: Some(width),
    }
}

/// The scope overview's input over a summary's `scopes` (name, count) and
/// `scope_edges` rows (a, b, count), summed into unordered pairs of
/// different scopes in order of first appearance.
pub fn build_overview_input(
    scopes: &[(&str, usize)],
    rows: &[(&str, &str, usize)],
    aspect: f64,
    width: f64,
) -> overview::OverviewInput {
    use std::collections::HashMap;
    let shown: Vec<(&str, usize)> = scopes.iter().copied().filter(|l| l.1 > 0).collect();
    let index: HashMap<&str, usize> = shown.iter().enumerate().map(|(i, l)| (l.0, i)).collect();
    let mut pairs: Vec<(&str, &str, usize)> = Vec::new();
    let mut at: HashMap<(&str, &str), usize> = HashMap::new();
    for &(ra, rb, count) in rows {
        if ra == rb {
            continue;
        }
        let (a, b) = if js_cmp(ra, rb).is_lt() {
            (ra, rb)
        } else {
            (rb, ra)
        };
        match at.get(&(a, b)) {
            Some(&k) => pairs[k].2 += count,
            None => {
                at.insert((a, b), pairs.len());
                pairs.push((a, b, count));
            }
        }
    }
    // `scopeName`: "No scope" for the empty one.
    let name_len = |name: &str| {
        if name.is_empty() {
            "No scope".len()
        } else {
            name.encode_utf16().count()
        }
    };
    overview::OverviewInput {
        nodes: shown
            .iter()
            .map(|&(name, count)| overview::OverviewNode {
                id: name.to_string(),
                count: count as f64,
                // "<name> · <count>"
                full_len: name_len(name) + 3 + fmt_len(count),
                short_len: name_len(name),
            })
            .collect(),
        edges: pairs
            .into_iter()
            .filter_map(|(a, b, n)| {
                Some(overview::OverviewEdge {
                    s: *index.get(a)?,
                    t: *index.get(b)?,
                    n: n as f64,
                })
            })
            .collect(),
        aspect,
        width,
    }
}

/// The widest box laid out, in px. `graph/input.js::roundWidth` clamps to
/// the same `MAX_LAYOUT_WIDTH`, so a page wider than this (an ultrawide
/// window zoomed out) still asks for a width the server accepts rather
/// than turning the whole `/graph` into a 400.
pub const MAX_LAYOUT_WIDTH: u32 = 20_000;

/// Which layout a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutName {
    ScopeForce,
    ScopeOverview,
    Grid,
}

impl LayoutName {
    pub fn as_str(self) -> &'static str {
        match self {
            LayoutName::ScopeForce => "scope-force",
            LayoutName::ScopeOverview => "scope-overview",
            LayoutName::Grid => "grid",
        }
    }
}

/// The `layout*=` query parameters of `/graph`. `layout=` names one or
/// more of `scope-overview`, `scope-force` or `grid` (the style's own,
/// grouped by top scope) and `opened` (an opened scope: scope-force grouped by sub-scope,
/// when `top_scope=` names one scope and more than `layout_open_over=N`
/// memories pass the status filters, which is when the page opens it). The
/// response carries the one that fits it (module docs of `graph.rs`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutRequest {
    pub overview: bool,
    pub force: bool,
    pub grid: bool,
    /// `opened`'s threshold.
    pub open_over: Option<usize>,
    /// Box height / width: a multiple of 0.05 from 0.3 to 3.
    pub aspect: f64,
    /// The box's width in px: a multiple of 40.
    pub width: f64,
}

impl LayoutRequest {
    /// `None` when no layout was asked for; an error for a bad parameter.
    pub fn parse(
        q: &std::collections::HashMap<String, String>,
    ) -> Result<Option<Self>, crate::ApiError> {
        let Some(names) = q.get("layout") else {
            return Ok(None);
        };
        let bad = crate::ApiError::bad_request;
        let (mut overview, mut force, mut grid, mut opened) = (false, false, false, false);
        for name in names.split(',') {
            match name {
                "scope-force" => force = true,
                "grid" => grid = true,
                "scope-overview" => overview = true,
                "opened" => opened = true,
                other => {
                    return Err(bad(format!(
                        "unknown layout {other:?} (the server lays out scope-force, grid, scope-overview and opened)"
                    )))
                }
            }
        }
        // As the page rounds them (graph/input.js): anything else would be
        // a new cache key per value.
        let aspect: f64 = q
            .get("layout_aspect")
            .and_then(|a| a.parse().ok())
            .filter(|a: &f64| {
                a.is_finite() && (0.3..=3.0).contains(a) && js_round(a * 20.0) / 20.0 == *a
            })
            .ok_or_else(|| bad("layout_aspect must be a multiple of 0.05 from 0.3 to 3".into()))?;
        let width: f64 = q
            .get("layout_width")
            .and_then(|w| w.parse::<u32>().ok())
            .filter(|w| *w <= MAX_LAYOUT_WIDTH && w % 40 == 0)
            .ok_or_else(|| bad("layout_width must be a whole multiple of 40 pixels".into()))?
            .into();
        let open_over = if opened {
            Some(
                q.get("layout_open_over")
                    .and_then(|n| n.parse::<usize>().ok())
                    .ok_or_else(|| {
                        bad("layout=opened needs layout_open_over, a number of memories".into())
                    })?,
            )
        } else {
            None
        };
        Ok(Some(Self {
            overview,
            force,
            grid,
            open_over,
            aspect,
            width,
        }))
    }
}

/// A layout's input, either kind.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum LayoutInput {
    Force(ForceInput),
    Overview(overview::OverviewInput),
    Grid(ForceInput),
}

impl LayoutInput {
    pub fn name(&self) -> LayoutName {
        match self {
            LayoutInput::Force(_) => LayoutName::ScopeForce,
            LayoutInput::Overview(_) => LayoutName::ScopeOverview,
            LayoutInput::Grid(_) => LayoutName::Grid,
        }
    }

    /// SHA-256 over the salt (layout version, crate
    /// version), the layout's name and the exact input (canonical JSON of
    /// the structs above), hex. Everything the cached result depends on is
    /// in it — every id, length, radius, edge end and the aspect; the width
    /// too for the overview, never for scope-force, whose cached core is
    /// width-free (module docs) — so equal digests mean equal layouts,
    /// whichever store they came from.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"engramdb-dashboard-layout\0");
        h.update(LAYOUT_VERSION.to_le_bytes());
        h.update(env!("CARGO_PKG_VERSION").as_bytes());
        h.update(b"\0");
        h.update(self.name().as_str().as_bytes());
        h.update(b"\0");
        let body = match self {
            LayoutInput::Force(i) | LayoutInput::Grid(i) => {
                serde_json::to_vec(&(&i.nodes, &i.edges, i.aspect))
            }
            LayoutInput::Overview(i) => serde_json::to_vec(i),
        };
        h.update(body.expect("a layout input serializes"));
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    }

    /// Run the layout: what the cache holds for it, or why it cannot be
    /// sent (a number that is not finite: never drawn, never cached).
    pub fn run(&self, key: &str) -> Result<Cached, String> {
        match self {
            LayoutInput::Force(input) => {
                let core = scope_force::layout_core(input);
                let finite = core.pos.iter().all(|v| v.is_finite())
                    && core.routes.iter().flatten().all(|v| v.is_finite())
                    && core
                        .labels
                        .iter()
                        .all(|l| l.x.is_finite() && l.y.is_finite())
                    && core.region_r.is_finite();
                if !finite {
                    return Err("scope-force produced a position that is not a number".into());
                }
                Ok(Cached::Force(Box::new(core)))
            }
            LayoutInput::Grid(input) => {
                let out = grid::layout(input);
                let finite = out.pos.iter().all(|v| v.is_finite())
                    && out.routes.iter().flatten().flatten().all(|v| v.is_finite())
                    && out
                        .lines
                        .iter()
                        .flat_map(|l| &l.pts)
                        .flatten()
                        .all(|v| v.is_finite())
                    && out
                        .labels
                        .iter()
                        .all(|l| l.x.is_finite() && l.y.is_finite())
                    && out.bounds.iter().all(|v| v.is_finite());
                if !finite {
                    return Err("the grid layout produced a position that is not a number".into());
                }
                Ok(Cached::Grid(encode_grid(&out, input, key).to_string()))
            }
            LayoutInput::Overview(input) => {
                let out = overview::layout(input);
                let b = &out.bounds;
                let finite = out.pos.iter().chain(&out.r).all(|v| v.is_finite())
                    && out.routes.iter().flatten().flatten().all(|v| v.is_finite())
                    && out
                        .labels
                        .iter()
                        .all(|l| l.x.is_finite() && l.y.is_finite())
                    && [b.x, b.y, b.w, b.h].iter().all(|v| v.is_finite());
                if !finite {
                    return Err(
                        "the scope overview produced a position that is not a number".into(),
                    );
                }
                Ok(Cached::Overview(
                    encode_overview(&out, input, key).to_string(),
                ))
            }
        }
    }

    /// The encoded layout for the page, as JSON text, from what the cache
    /// holds: scope-force finished for this input's width (with `dirs`, an
    /// opened scope's far-scope directions). An error when `cached` is the
    /// other kind (a digest collision in name only; never expected) or the
    /// finished names are not finite.
    pub fn render(
        &self,
        cached: &Cached,
        key: &str,
        groups: Groups,
        dirs: Option<&Value>,
    ) -> Result<String, String> {
        match (self, cached) {
            (LayoutInput::Force(input), Cached::Force(core)) => {
                let out = scope_force::finish(core, input.width);
                let finite = out.bounds.iter().all(|v| v.is_finite())
                    && out
                        .scopes
                        .iter()
                        .all(|s| [s.x, s.y, s.w, s.size].iter().all(|v| v.is_finite()));
                if !finite {
                    return Err("scope-force placed a scope name off the map".into());
                }
                Ok(encode_force(&out, input, key, groups, dirs).to_string())
            }
            (LayoutInput::Overview(_), Cached::Overview(text))
            | (LayoutInput::Grid(_), Cached::Grid(text)) => Ok(text.clone()),
            _ => Err("the cached layout is of another kind".into()),
        }
    }
}

/// Standard base64 with padding (what `atob` reads).
pub fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(A[(v >> 18) as usize & 63] as char);
        out.push(A[(v >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[v as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn f64s(vals: impl IntoIterator<Item = f64>) -> String {
    let mut bytes = Vec::new();
    for v in vals {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    base64(&bytes)
}

/// scope-force's output for the page (module docs, "Encoding"): arrays
/// parallel to the response's `nodes` and `edges`.
pub fn encode_force(
    out: &ForceOutput,
    input: &ForceInput,
    key: &str,
    groups: Groups,
    dirs: Option<&Value>,
) -> Value {
    let mut group_keys: Vec<&str> = Vec::new();
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let group: Vec<usize> = input
        .nodes
        .iter()
        .map(|n| {
            *index.entry(n.top_scope.as_str()).or_insert_with(|| {
                group_keys.push(n.top_scope.as_str());
                group_keys.len() - 1
            })
        })
        .collect();
    let flags: Vec<u8> = out
        .labels
        .iter()
        .map(|l| anchor_byte(l.anchor) | if l.placed { 0x80 } else { 0 })
        .collect();
    let mut v = json!({
        "v": LAYOUT_VERSION,
        "name": LayoutName::ScopeForce.as_str(),
        "key": key,
        "groups": groups.as_str(),
        "aspect": input.aspect,
        "width": input.width,
        "pos": f64s(out.pos.iter().copied()),
        "routes": f64s(out.routes.iter().flatten().copied()),
        "label_xy": f64s(out.labels.iter().flat_map(|l| [l.x, l.y])),
        "label_flags": base64(&flags),
        "group_keys": group_keys,
        "group": group,
        "bounds": { "x": out.bounds[0], "y": out.bounds[1], "w": out.bounds[2], "h": out.bounds[3] },
        "scopes": out.scopes.iter().map(|s| json!({
            "key": s.key, "slot": s.slot, "x": s.x, "y": s.y, "w": s.w,
            "size": s.size, "placed": s.placed, "backed": s.backed,
        })).collect::<Vec<_>>(),
        "region_r": out.region_r,
    });
    if let Some(d) = dirs {
        v["dirs"] = d.clone();
    }
    v
}

/// Grid's output for the page: `pos`, `label_xy`, `label_flags` as for
/// scope-force; `routes` as the points of every relation's polyline
/// (`route_points`, one byte per relation: how many points it has, 2 or 3;
/// `routes`, their doubles in order), `chains` (each group's chain: `key`,
/// `slot`, `pts`), `bounds`.
pub fn encode_grid(out: &grid::GridOutput, input: &ForceInput, key: &str) -> Value {
    let flags: Vec<u8> = out
        .labels
        .iter()
        .map(|l| anchor_byte(l.anchor) | if l.placed { 0x80 } else { 0 })
        .collect();
    let counts: Vec<u8> = out
        .routes
        .iter()
        .map(|r| u8::try_from(r.len()).unwrap_or(u8::MAX))
        .collect();
    json!({
        "v": LAYOUT_VERSION,
        "name": LayoutName::Grid.as_str(),
        "key": key,
        "aspect": input.aspect,
        "width": Value::Null,
        "pos": f64s(out.pos.iter().copied()),
        "route_points": base64(&counts),
        "routes": f64s(out.routes.iter().flatten().flatten().copied()),
        "label_xy": f64s(out.labels.iter().flat_map(|l| [l.x, l.y])),
        "label_flags": base64(&flags),
        "chains": out.lines.iter().map(|l| json!({
            "key": l.key, "slot": l.slot, "pts": l.pts,
        })).collect::<Vec<_>>(),
        "bounds": { "x": out.bounds[0], "y": out.bounds[1], "w": out.bounds[2], "h": out.bounds[3] },
    })
}

/// The overview's output for the page: the layout's output with the scope
/// name of each disc (`ids`) and the two scope names of each band (`pairs`),
/// so the page matches them by name.
pub fn encode_overview(
    out: &overview::OverviewOutput,
    input: &overview::OverviewInput,
    key: &str,
) -> Value {
    let id = |i: usize| input.nodes.get(i).map_or("", |n| n.id.as_str());
    json!({
        "v": LAYOUT_VERSION,
        "name": LayoutName::ScopeOverview.as_str(),
        "key": key,
        "aspect": input.aspect,
        "width": input.width,
        "ids": input.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        "pairs": input.edges.iter().map(|e| [id(e.s), id(e.t)]).collect::<Vec<_>>(),
        "pos": out.pos,
        "r": out.r,
        "routes": out.routes,
        "labels": out.labels,
        "bounds": out.bounds,
    })
}

/// An opened scope's far-scope directions, from the scope overview's
/// encoded JSON (`text`, [`encode_overview`]): far scope name → the vector
/// from `opened`'s disc to its disc. Only the names in `far` that have a
/// disc; `None` when `opened` has none.
pub fn overview_dirs(text: &str, opened: &str, far: &[&str]) -> Option<Value> {
    let v: Value = serde_json::from_str(text).ok()?;
    let ids = v["ids"].as_array()?;
    let pos = v["pos"].as_array()?;
    let at = |name: &str| -> Option<(f64, f64)> {
        let i = ids.iter().position(|x| x.as_str() == Some(name))?;
        Some((pos.get(2 * i)?.as_f64()?, pos.get(2 * i + 1)?.as_f64()?))
    };
    let (mx, my) = at(opened)?;
    let mut out = serde_json::Map::new();
    for name in far {
        if let Some((x, y)) = at(name) {
            out.insert((*name).to_string(), json!([x - mx, y - my]));
        }
    }
    Some(Value::Object(out))
}

#[cfg(test)]
mod tests;
