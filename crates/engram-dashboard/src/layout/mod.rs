//! Server-side graph layouts: the page's core JS layouts, run in Rust and
//! sent with `/graph` so the page draws without laying out.
//!
//! **What.** Ports of `web/js/graph/scope-force.js` ([`scope_force`], Plain's
//! layout and every opened scope's), `web/js/graph/labels.js` ([`labels`])
//! and `web/js/graph/scope-overview.js` ([`overview`]). Grid's layout stays
//! in JS only. The JS modules stay the reference and the page's fallback: a
//! response without a `layout` (not asked for, timed out, mismatched) is
//! laid out in the page's worker exactly as before.
//!
//! **Same bits.** Each port follows its JS statement for statement: the
//! same IEEE double operations in the same order (Rust never fuses a
//! multiply-add on its own; `sqrt` is correctly rounded in both), the same
//! iteration orders (memories by id, maps in insertion order, computed
//! numeric map keys kept as the same doubles), `Math.round` as
//! [`js_round`], string order as UTF-16 code units ([`js_cmp`]) and text
//! lengths in UTF-16 units. So the server's layout is the worker's, bit for
//! bit, which `bench/server-layout.mjs` checks on real stores and the unit
//! tests pin on fixtures.
//!
//! **The input.** Built by `graph.rs` from the very response it sends, the
//! way `views/graph.js` builds the worker's input from it: node order,
//! `top_scope` (or the sub-scope key, [`sub_key`], for an opened scope),
//! radius from reads ([`radius`]), the label's length ([`short_label_len`]),
//! edges with stub ends at `-1`. The box's `aspect` (rounded to 0.05) and
//! `width` (rounded to 40 px) come from the page (`layout_aspect=`,
//! `layout_width=`); the width is part of the input because the drawn scope
//! names (scope-force's step 5b) and the overview's whole geometry depend
//! on it. Keeping a width-free result and re-placing the names in the page
//! would save one cache entry per width bucket and cost a second name
//! placement on the main thread, so the server takes it.
//!
//! **Encoding** ([`encode_force`], `layout.v` = [`LAYOUT_VERSION`]): the
//! positions as little-endian `f64` (base64), exact rather than rounded to
//! integers, so the page draws what its worker would have to the last bit;
//! one byte per label (its spot index, `255` for unplaced: the page rebuilds
//! `x`, `y`, `anchor` from the position, radius and width with
//! `labels.js::spotsAt`); only the stub ends of the routes (a straight
//! relation's route is its two positions); bounds, scope names and
//! `region_r` as JSON. The overview is small and goes as plain JSON.
//!
//! **Cache** ([`cache`]): in memory, and on disk under the OS cache dir —
//! the dashboard's one declared write outside a store (see `cache.rs`).

pub mod cache;
pub mod labels;
pub mod overview;
pub mod scope_force;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The encoding and algorithm version: part of every digest, so a change to
/// either (bump it) never serves an old cached layout.
pub const LAYOUT_VERSION: u32 = 1;

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

/// The UTF-16 length of `views/graph.js::shortLabel(summary)` (24 units at
/// most plus an ellipsis): all a layout reads of a label is its width.
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

/// `graph/radius.js::radius`: 5, growing with reads up to 11.
pub fn radius(reads: u64) -> f64 {
    5.0 + 6f64.min((reads as f64).sqrt() * 0.55)
}

/// `graph/opened.js::subKey`: an opened scope's region key, the first two
/// segments of the memory's scope, else its top scope.
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

/// One memory of scope-force's input (`views/graph.js`'s node, minus the
/// label text: the layout reads only its length).
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

/// Which layout a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutName {
    ScopeForce,
    ScopeOverview,
}

impl LayoutName {
    pub fn as_str(self) -> &'static str {
        match self {
            LayoutName::ScopeForce => "scope-force",
            LayoutName::ScopeOverview => "scope-overview",
        }
    }
}

/// The `layout*=` query parameters of `/graph`. `layout=` names one or
/// both layouts (comma-separated); the response carries the one that fits
/// it (the overview for a summary, scope-force for a graph).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutRequest {
    pub overview: bool,
    pub force: bool,
    /// Box height / width, as the page rounded it (0.3–3).
    pub aspect: f64,
    /// The box's width in px, as the page rounded it.
    pub width: f64,
    /// Group memories by their sub-scope (an opened scope).
    pub sub_groups: bool,
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
        let (mut overview, mut force) = (false, false);
        for name in names.split(',') {
            match name {
                "scope-force" => force = true,
                "scope-overview" => overview = true,
                other => {
                    return Err(bad(format!(
                    "unknown layout {other:?} (the server lays out scope-force and scope-overview)"
                )))
                }
            }
        }
        let aspect: f64 = q
            .get("layout_aspect")
            .and_then(|a| a.parse().ok())
            .filter(|a: &f64| a.is_finite() && (0.3..=3.0).contains(a))
            .ok_or_else(|| bad("layout_aspect must be a number from 0.3 to 3".into()))?;
        let width: f64 = q
            .get("layout_width")
            .and_then(|w| w.parse::<u32>().ok())
            .filter(|w| *w <= 20_000)
            .ok_or_else(|| bad("layout_width must be a whole number of pixels".into()))?
            .into();
        let sub_groups = match q.get("layout_groups").map(String::as_str) {
            None | Some("top") => false,
            Some("sub") => true,
            Some(other) => {
                return Err(bad(format!(
                    "layout_groups must be top or sub, not {other:?}"
                )))
            }
        };
        Ok(Some(Self {
            overview,
            force,
            aspect,
            width,
            sub_groups,
        }))
    }
}

/// A layout's input, either kind.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum LayoutInput {
    Force(ForceInput),
    Overview(overview::OverviewInput),
}

impl LayoutInput {
    pub fn name(&self) -> LayoutName {
        match self {
            LayoutInput::Force(_) => LayoutName::ScopeForce,
            LayoutInput::Overview(_) => LayoutName::ScopeOverview,
        }
    }

    /// SHA-256 over the version, the layout's name and the exact input
    /// (canonical JSON of the structs above), hex. Everything the layout
    /// reads is in it — every id, length, radius, edge end, the aspect and
    /// the width — so equal digests mean equal layouts, whichever store
    /// they came from.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"engramdb-dashboard-layout\0");
        h.update(LAYOUT_VERSION.to_le_bytes());
        h.update(self.name().as_str().as_bytes());
        h.update(b"\0");
        h.update(serde_json::to_vec(self).expect("a layout input serializes"));
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    }

    /// Run the layout and encode it for the page, as JSON text (serde_json
    /// prints every double exactly; see `cache::Encoded` for why it stays
    /// text).
    pub fn run(&self, key: &str) -> String {
        let v = match self {
            LayoutInput::Force(input) => encode_force(&scope_force::layout(input), input, key),
            LayoutInput::Overview(input) => encode_overview(&overview::layout(input), input, key),
        };
        v.to_string()
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

/// scope-force's output for the page (module docs, "Encoding").
pub fn encode_force(out: &ForceOutput, input: &ForceInput, key: &str) -> Value {
    let n = input.nodes.len() as i64;
    let valid = |v: i64| v >= 0 && v < n;
    let stubs = input
        .edges
        .iter()
        .zip(&out.routes)
        .filter(|(e, _)| valid(e.s) && !valid(e.t))
        .flat_map(|(_, r)| [r[2], r[3]]);
    let spots: Vec<u8> = out
        .labels
        .iter()
        .map(|l| if l.placed { l.spot } else { 255 })
        .collect();
    json!({
        "v": LAYOUT_VERSION,
        "name": LayoutName::ScopeForce.as_str(),
        "key": key,
        "n": input.nodes.len(),
        "e": input.edges.len(),
        "aspect": input.aspect,
        "width": input.width,
        "pos": f64s(out.pos.iter().copied()),
        "labels": base64(&spots),
        "stubs": f64s(stubs),
        "bounds": { "x": out.bounds[0], "y": out.bounds[1], "w": out.bounds[2], "h": out.bounds[3] },
        "scopes": out.scopes.iter().map(|s| json!({
            "key": s.key, "slot": s.slot, "x": s.x, "y": s.y, "w": s.w,
            "size": s.size, "placed": s.placed, "backed": s.backed,
        })).collect::<Vec<_>>(),
        "region_r": out.region_r,
    })
}

/// The overview's output for the page: the JS output as is, plus the
/// header fields the page checks.
pub fn encode_overview(
    out: &overview::OverviewOutput,
    input: &overview::OverviewInput,
    key: &str,
) -> Value {
    json!({
        "v": LAYOUT_VERSION,
        "name": LayoutName::ScopeOverview.as_str(),
        "key": key,
        "n": input.nodes.len(),
        "e": input.edges.len(),
        "aspect": input.aspect,
        "width": input.width,
        "pos": out.pos,
        "r": out.r,
        "routes": out.routes,
        "labels": out.labels,
        "bounds": out.bounds,
    })
}

#[cfg(test)]
mod tests;
