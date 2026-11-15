//! Unit tests of the server's layouts: the output contract and
//! determinism (on a generated graph and on `/graph`-shaped fixtures in
//! `fixtures/`: long and non-ASCII scope names and summaries, stubs, an
//! opened scope's sub-scopes, a 14-scope overview), the label and disc
//! geometry, the helpers' JS semantics (the page draws the label text and
//! radius these measure), the encoding and the cache.

use super::*;

fn node(id: &str, top: &str, scope: &str, degree: f64, reads: u64) -> ForceNode {
    ForceNode {
        id: id.to_string(),
        top_scope: top.to_string(),
        slot: None,
        scope: scope.to_string(),
        degree,
        r: radius(reads),
        label_len: 12,
        weight: reads as f64,
    }
}

/// A small deterministic graph: three scopes, a ring inside each, a few
/// cross-scope relations and two stubs.
fn sample(n: usize) -> ForceInput {
    let tops = ["storage", "dashboard", ""];
    let nodes: Vec<ForceNode> = (0..n)
        .map(|i| {
            let top = tops[i % 3];
            let scope = if top.is_empty() {
                String::new()
            } else {
                format!("{top}.s{}", i % 2)
            };
            node(
                &format!("{:016x}", i * 7919),
                top,
                &scope,
                2.0,
                (i % 5) as u64,
            )
        })
        .collect();
    let mut edges = Vec::new();
    for i in 0..n {
        let j = (i + 3) % n;
        edges.push(ForceEdge {
            s: i as i64,
            t: j as i64,
            far_scope: Some(nodes[j].top_scope.clone()),
        });
    }
    edges.push(ForceEdge {
        s: 0,
        t: -1,
        far_scope: None,
    });
    edges.push(ForceEdge {
        s: 0,
        t: -1,
        far_scope: None,
    });
    ForceInput {
        nodes,
        edges,
        aspect: 0.6,
        width: Some(1280.0),
    }
}

#[test]
fn js_helpers_follow_js() {
    assert_eq!(js_round(2.5), 3.0);
    assert_eq!(js_round(-2.5), -2.0);
    assert_eq!(js_round(0.49999999999999994), 0.0);
    assert_eq!(js_cmp("a", "b"), std::cmp::Ordering::Less);
    // UTF-16 order: U+FF61 sorts after U+1F600's high surrogate (0xD83D).
    assert_eq!(js_cmp("\u{1F600}", "\u{FF61}"), std::cmp::Ordering::Less);
    assert_eq!(fkey(-0.0), fkey(0.0));
    assert_eq!(base64(b"hello"), "aGVsbG8=");
    assert_eq!(base64(b"hi"), "aGk=");
    assert_eq!(base64(b"abc"), "YWJj");
    assert_eq!(fmt_len(999), 3);
    assert_eq!(fmt_len(1000), 5);
    assert_eq!(fmt_len(1_234_567), 9);
    assert_eq!(sub_key("storage.lock.deep", "storage"), "storage.lock");
    assert_eq!(sub_key("storage", "storage"), "storage");
    assert_eq!(sub_key("", ""), "");
}

#[test]
fn short_label_matches_the_js() {
    let len = |s: &str| short_label_len(s);
    let text = |s: &str| String::from_utf16(&short_label(s)).unwrap();
    assert_eq!(text("  Short one  "), "Short one");
    assert_eq!(
        text("Use flock for the write lock: it is advisory"),
        "Use flock for the write…"
    );
    assert_eq!(
        text("A rule, with a comma, and more words after it"),
        "A rule, with a comma…"
    );
    assert_eq!(text("Short: cut"), "Short: cut"); // a cut at ≤ 8 is ignored
    assert_eq!(
        text("averyveryverylongwordwithoutanyspaceatall"),
        "averyveryverylongwordwit…"
    );
    assert_eq!(len(""), 0);
    // Non-ASCII counts in UTF-16 units: "é" is one, "😀" two.
    assert_eq!(len("é"), 1);
    assert_eq!(len("😀"), 2);
}

#[test]
fn scope_force_honours_the_contract_and_is_deterministic() {
    for n in [0usize, 1, 2, 7, 60, 400] {
        let input = sample(n.max(1)).clone();
        let input = if n == 0 {
            ForceInput {
                nodes: Vec::new(),
                edges: Vec::new(),
                ..input
            }
        } else {
            input
        };
        let a = scope_force::layout(&input);
        let b = scope_force::layout(&input);
        assert_eq!(a, b, "deterministic at n = {n}");
        assert_eq!(a.pos.len(), 2 * input.nodes.len());
        assert!(a.pos.iter().all(|v| v.is_finite()));
        assert_eq!(a.routes.len(), input.edges.len());
        assert!(a.routes.iter().flatten().all(|v| v.is_finite()));
        assert_eq!(a.labels.len(), input.nodes.len());
        assert!(a.bounds.iter().all(|v| v.is_finite()) && a.bounds[2] > 0.0);
        // Positions do not depend on the width (only the drawn names do).
        let narrow = scope_force::layout(&ForceInput {
            width: Some(360.0),
            ..input.clone()
        });
        assert_eq!(a.pos, narrow.pos, "positions are width-free at n = {n}");
    }
}

#[test]
fn overview_honours_the_contract() {
    let input = overview::OverviewInput {
        nodes: (0..5)
            .map(|i| overview::OverviewNode {
                id: format!("scope{i}"),
                count: (100 / (i + 1)) as f64,
                full_len: 12,
                short_len: 6,
            })
            .collect(),
        edges: vec![
            overview::OverviewEdge { s: 0, t: 1, n: 5.0 },
            overview::OverviewEdge { s: 2, t: 4, n: 1.0 },
        ],
        aspect: 0.55,
        width: 1240.0,
    };
    let a = overview::layout(&input);
    assert_eq!(a, overview::layout(&input));
    assert_eq!(a.pos.len(), 10);
    assert_eq!(a.routes.len(), 2);
    assert!(a.labels.iter().all(|l| l.placed));
    assert_eq!(a.bounds.w, 1240.0);
    assert_eq!(a.bounds.h, js_round(1240.0 * 0.55));
}

#[test]
fn same_input_same_digest_and_any_change_moves_it() {
    let a = LayoutInput::Force(sample(30));
    let b = LayoutInput::Force(sample(30));
    assert_eq!(a.digest(), b.digest());
    assert_eq!(a.digest().len(), 64);
    let mut changed = sample(30);
    changed.aspect = 0.65;
    assert_ne!(a.digest(), LayoutInput::Force(changed).digest());
    let mut changed = sample(30);
    changed.nodes[3].label_len += 1;
    assert_ne!(a.digest(), LayoutInput::Force(changed).digest());
    // The width is not in scope-force's digest: its cached core is width-free.
    let mut narrow = sample(30);
    narrow.width = Some(360.0);
    assert_eq!(a.digest(), LayoutInput::Force(narrow).digest());
}

#[test]
fn the_page_never_asks_for_a_width_the_server_refuses() {
    let js = std::str::from_utf8(crate::assets::find("js/graph/input.js").unwrap().bytes).unwrap();
    assert!(
        js.contains(&format!(
            "export const MAX_LAYOUT_WIDTH = {MAX_LAYOUT_WIDTH};"
        )),
        "graph/input.js must clamp roundWidth to the server's MAX_LAYOUT_WIDTH"
    );
    // Both ends: the server refuses under 40 too (E13).
    assert!(js.contains("Math.min(MAX_LAYOUT_WIDTH, Math.max(40, w))"));
    assert_eq!(MAX_LAYOUT_WIDTH % 40, 0, "the cap is itself a valid width");
}

#[test]
fn the_core_is_width_free_and_finishes_per_width() {
    for n in [1usize, 60, 400] {
        let input = sample(n);
        let core = scope_force::layout_core(&input);
        for w in [360.0, 1280.0, 3000.0] {
            let wide = ForceInput {
                width: Some(w),
                ..input.clone()
            };
            assert_eq!(
                scope_force::finish(&core, Some(w)),
                scope_force::layout(&wide),
                "n = {n}, width {w}"
            );
        }
    }
}

#[test]
fn the_cached_form_round_trips_and_refuses_garbage() {
    let core = Cached::Force(Box::new(scope_force::layout_core(&sample(80))));
    let bytes = core.to_bytes();
    assert_eq!(Cached::from_bytes(&bytes).as_ref(), Some(&core));
    let ov = Cached::Overview("{\"v\":2}".into());
    assert_eq!(Cached::from_bytes(&ov.to_bytes()), Some(ov));
    // Truncated, padded, or a huge length: a miss, never a panic.
    for cut in [0, 1, 9, bytes.len() / 2, bytes.len() - 1] {
        assert!(Cached::from_bytes(&bytes[..cut]).is_none(), "cut at {cut}");
    }
    let mut long = bytes.clone();
    long.push(0);
    assert!(Cached::from_bytes(&long).is_none());
    let mut huge = vec![0u8];
    huge.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(Cached::from_bytes(&huge).is_none());
}

fn unb64(s: &str) -> Vec<u8> {
    const A: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut have = 0;
    let mut out = Vec::new();
    for c in s.chars().filter(|&c| c != '=') {
        bits = (bits << 6) | A.find(c).unwrap() as u32;
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
        }
    }
    out
}

fn unb64_f64(s: &str) -> Vec<f64> {
    unb64(s)
        .chunks(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// A scope-force input as `/graph` sends it, as text (exact: serde_json,
/// built without `float_roundtrip`, may round a parsed double's last bit).
fn rendered_text(input: &ForceInput, groups: Groups) -> String {
    let li = LayoutInput::Force(input.clone());
    let key = li.digest();
    let cached = li.run(&key).unwrap();
    li.render(&cached, &key, groups, None).unwrap()
}

fn rendered(input: &ForceInput, groups: Groups) -> Value {
    serde_json::from_str(&rendered_text(input, groups)).unwrap()
}

#[test]
fn the_encoding_is_parallel_to_the_response() {
    let input = sample(40);
    let out = scope_force::layout(&input);
    let v = rendered(&input, Groups::Sub);
    assert_eq!(v["v"], LAYOUT_VERSION);
    assert_eq!(v["name"], "scope-force");
    assert_eq!(v["groups"], "sub");
    assert_eq!(unb64_f64(v["pos"].as_str().unwrap()), out.pos);
    let routes = unb64_f64(v["routes"].as_str().unwrap());
    assert_eq!(routes.len(), 4 * input.edges.len());
    assert_eq!(
        routes,
        out.routes.iter().flatten().copied().collect::<Vec<_>>()
    );
    let xy = unb64_f64(v["label_xy"].as_str().unwrap());
    let flags = unb64(v["label_flags"].as_str().unwrap());
    assert_eq!(flags.len(), 40);
    for (i, l) in out.labels.iter().enumerate() {
        assert_eq!((xy[2 * i], xy[2 * i + 1]), (l.x, l.y));
        assert_eq!(flags[i] & 0x80 != 0, l.placed);
        assert_eq!(labels::Anchor::ALL[usize::from(flags[i] & 0x7f)], l.anchor);
    }
    // Each memory's region by index into `group_keys`.
    let keys: Vec<&str> = v["group_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    for (n, g) in input.nodes.iter().zip(v["group"].as_array().unwrap()) {
        assert_eq!(keys[g.as_u64().unwrap() as usize], n.top_scope);
    }
    assert!(v.get("dirs").is_none());
}

/// Two labels' boxes overlap (a hairline touch is not an overlap).
fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x0 < b.x1 - 1e-9 && b.x0 < a.x1 - 1e-9 && a.y0 < b.y1 - 1e-9 && b.y0 < a.y1 - 1e-9
}

/// The contract every scope-force output keeps: one finite position per
/// memory, one finite route of two points per relation starting at its
/// source, one label per memory, placed labels never overlapping each other
/// or a memory, finite bounds holding every memory.
fn check_force_contract(name: &str, input: &ForceInput, out: &ForceOutput) {
    let n = input.nodes.len();
    assert_eq!(out.pos.len(), 2 * n, "{name}");
    assert!(out.pos.iter().all(|v| v.is_finite()), "{name}: positions");
    assert_eq!(out.routes.len(), input.edges.len(), "{name}");
    for (k, (r, e)) in out.routes.iter().zip(&input.edges).enumerate() {
        assert!(r.iter().all(|v| v.is_finite()), "{name}: route {k}");
        if e.s >= 0 {
            let s = e.s as usize;
            assert_eq!(
                [r[0], r[1]],
                [out.pos[2 * s], out.pos[2 * s + 1]],
                "{name}: route {k} source"
            );
        }
        if e.t >= 0 {
            let t = e.t as usize;
            assert_eq!(
                [r[2], r[3]],
                [out.pos[2 * t], out.pos[2 * t + 1]],
                "{name}: route {k} target"
            );
        }
    }
    assert_eq!(out.labels.len(), n, "{name}");
    let placed: Vec<&labels::Label> = out.labels.iter().filter(|l| l.placed).collect();
    assert!(!placed.is_empty() || n == 0, "{name}: no label placed");
    for (a, la) in placed.iter().enumerate() {
        assert!(la.x.is_finite() && la.y.is_finite(), "{name}: label");
        for lb in &placed[a + 1..] {
            assert!(
                !overlaps(&la.rect, &lb.rect),
                "{name}: labels overlap: {la:?} {lb:?}"
            );
        }
    }
    let [bx, by, bw, bh] = out.bounds;
    assert!(
        [bx, by, bw, bh].iter().all(|v| v.is_finite()) && bw > 0.0 && bh > 0.0,
        "{name}"
    );
    for i in 0..n {
        let (x, y) = (out.pos[2 * i], out.pos[2 * i + 1]);
        assert!(
            x >= bx && x <= bx + bw && y >= by && y <= by + bh,
            "{name}: memory {i} outside the bounds"
        );
    }
    assert!(out.region_r.is_finite() && out.region_r > 0.0, "{name}");
}

#[test]
fn scope_force_keeps_its_contract_on_a_generated_graph() {
    for n in [1usize, 2, 7, 60, 400] {
        let input = sample(n);
        check_force_contract(&format!("sample {n}"), &input, &scope_force::layout(&input));
    }
}

/// A fixture's response fields, as `graph.rs` builds the input from them.
fn fixture_force(fx: &Value) -> ForceInput {
    let nodes: Vec<NodeFacts<'_>> = fx["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| NodeFacts {
            id: n["id"].as_str().unwrap(),
            top_scope: n["top_scope"].as_str(),
            scope: n["scope"].as_str(),
            summary: n["summary"].as_str().unwrap(),
            reads: n["reads"].as_u64().unwrap(),
            degree: n["degree"].as_u64().unwrap() as usize,
            slot: n["color_slot"].as_u64().map(|s| s as u32),
        })
        .collect();
    let edges: Vec<EdgeFacts<'_>> = fx["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| EdgeFacts {
            src: e["src"].as_str().unwrap(),
            dst: e["dst"].as_str().unwrap(),
            cross_store: e["dst_store"].is_string(),
        })
        .collect();
    let groups = if fx["groups"] == "sub" {
        Groups::Sub
    } else {
        Groups::Top
    };
    build_force_input(
        &nodes,
        &edges,
        groups,
        fx["aspect"].as_f64().unwrap(),
        fx["width"].as_f64().unwrap(),
    )
}

fn fixture_overview(fx: &Value) -> overview::OverviewInput {
    let scopes: Vec<(&str, usize)> = fx["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["name"].as_str().unwrap(),
                l["count"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    let rows: Vec<(&str, &str, usize)> = fx["scope_edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["a"].as_str().unwrap(),
                r["b"].as_str().unwrap(),
                r["count"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    build_overview_input(
        &scopes,
        &rows,
        fx["aspect"].as_f64().unwrap(),
        fx["width"].as_f64().unwrap(),
    )
}

/// `/graph`-shaped inputs: a store's nodes and edges (by top scope and an
/// opened scope by sub-scope) and a summary's scopes and scope pairs.
const FIXTURES: &[(&str, &str)] = &[
    ("force-top", include_str!("fixtures/force-top.json")),
    ("force-sub", include_str!("fixtures/force-sub.json")),
    ("overview", include_str!("fixtures/overview.json")),
];

fn check_overview_contract(
    name: &str,
    input: &overview::OverviewInput,
    out: &overview::OverviewOutput,
) {
    let m = input.nodes.len();
    assert_eq!(out.pos.len(), 2 * m, "{name}");
    assert_eq!(out.r.len(), m, "{name}");
    assert!(
        out.pos.iter().chain(&out.r).all(|v| v.is_finite()),
        "{name}"
    );
    assert_eq!(out.routes.len(), input.edges.len(), "{name}");
    assert!(
        out.routes.iter().flatten().flatten().all(|v| v.is_finite()),
        "{name}"
    );
    assert_eq!(out.labels.len(), m, "{name}");
    // Discs never overlap one another, and stay in the box.
    for i in 0..m {
        assert!(out.r[i] > 0.0, "{name}: disc {i}");
        let (x, y, r) = (out.pos[2 * i], out.pos[2 * i + 1], out.r[i]);
        let b = &out.bounds;
        assert!(
            x - r >= b.x - 1e-6 && x + r <= b.x + b.w + 1e-6,
            "{name}: disc {i} outside"
        );
        assert!(
            y - r >= b.y - 1e-6 && y + r <= b.y + b.h + 1e-6,
            "{name}: disc {i} outside"
        );
        for j in i + 1..m {
            let d = ((out.pos[2 * i] - out.pos[2 * j]).powi(2)
                + (out.pos[2 * i + 1] - out.pos[2 * j + 1]).powi(2))
            .sqrt();
            // A disc drawn at the minimum radius (6 px) is larger than its
            // count's area: with very many scopes two such may touch.
            if out.r[i] > 6.0 && out.r[j] > 6.0 {
                assert!(
                    d >= out.r[i] + out.r[j] - 1e-6,
                    "{name}: discs {i} and {j} overlap: d {d}, radii {} + {}",
                    out.r[i],
                    out.r[j]
                );
            }
        }
    }
    assert!(
        out.labels
            .iter()
            .all(|l| l.x.is_finite() && l.y.is_finite()),
        "{name}"
    );
}

#[test]
fn the_layouts_keep_their_contract_and_are_deterministic_on_the_fixtures() {
    for (name, text) in FIXTURES {
        let fx: Value = serde_json::from_str(text).unwrap();
        match fx["layout"].as_str().unwrap() {
            "scope-force" => {
                let input = fixture_force(&fx);
                assert!(input.nodes.len() > 100, "{name}");
                assert!(input.edges.iter().any(|e| e.t < 0), "{name}: has stubs");
                let out = scope_force::layout(&input);
                check_force_contract(name, &input, &out);
                // Same input, same bytes: computed twice, and through the
                // cache's binary form.
                let groups = if fx["groups"] == "sub" {
                    Groups::Sub
                } else {
                    Groups::Top
                };
                let a = rendered_text(&input, groups);
                let b = rendered_text(&input, groups);
                assert_eq!(a, b, "{name}: deterministic");
                let li = LayoutInput::Force(input.clone());
                let key = li.digest();
                let cached = li.run(&key).unwrap();
                let back = Cached::from_bytes(&cached.to_bytes()).unwrap();
                assert_eq!(
                    li.render(&back, &key, groups, None).unwrap(),
                    a,
                    "{name}: the cached form renders the same bytes"
                );
            }
            "scope-overview" => {
                let input = fixture_overview(&fx);
                assert!(input.nodes.len() > 10, "{name}");
                let out = overview::layout(&input);
                check_overview_contract(name, &input, &out);
                let li = LayoutInput::Overview(input.clone());
                let key = li.digest();
                assert_eq!(
                    li.run(&key).unwrap(),
                    li.run(&key).unwrap(),
                    "{name}: deterministic"
                );
                // Every disc and band is named.
                let Cached::Overview(text) = li.run(&key).unwrap() else {
                    panic!("an overview");
                };
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["ids"].as_array().unwrap().len(), input.nodes.len());
                let pairs = v["pairs"].as_array().unwrap();
                assert_eq!(pairs.len(), input.edges.len());
                for (p, e) in pairs.iter().zip(&input.edges) {
                    assert_eq!(p[0], input.nodes[e.s].id.as_str());
                    assert_eq!(p[1], input.nodes[e.t].id.as_str());
                }
                // An opened scope's directions come from these discs.
                let me = input.nodes[0].id.as_str();
                let far: Vec<&str> = input.nodes[1..].iter().map(|n| n.id.as_str()).collect();
                let dirs = overview_dirs(&text, me, &far).unwrap();
                assert_eq!(dirs.as_object().unwrap().len(), far.len());
                assert!(overview_dirs(&text, "no such scope", &far).is_none());
            }
            other => panic!("{name}: unknown layout {other}"),
        }
    }
}

#[test]
fn overview_discs_never_overlap_however_many_scopes() {
    // Up to `PACK_MAX` scopes the discs are packed; past it they sit on a
    // sunflower spiral of their average size, where large ones can touch.
    for m in [1usize, 2, 5, 30, scope_force::PACK_MAX] {
        let input = overview::OverviewInput {
            nodes: (0..m)
                .map(|i| overview::OverviewNode {
                    id: format!("scope-{i}"),
                    count: (1 + (i * 37) % 500) as f64,
                    full_len: 14,
                    short_len: 7,
                })
                .collect(),
            edges: (1..m)
                .map(|i| overview::OverviewEdge {
                    s: i - 1,
                    t: i,
                    n: (i % 9 + 1) as f64,
                })
                .collect(),
            aspect: 0.6,
            width: 1240.0,
        };
        check_overview_contract(&format!("{m} scopes"), &input, &overview::layout(&input));
    }
}

#[test]
fn a_layout_that_is_not_finite_is_an_error_never_sent() {
    let mut input = sample(10);
    input.nodes[3].r = f64::NAN;
    input.nodes[4].weight = f64::INFINITY;
    let li = LayoutInput::Force(input);
    let key = li.digest();
    // Whatever the layout makes of it, a non-finite number never reaches
    // the page: either it is still finite or `run` refuses.
    if let Ok(c) = li.run(&key) {
        let text = li.render(&c, &key, Groups::Top, None).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(unb64_f64(v["pos"].as_str().unwrap())
            .iter()
            .all(|x| x.is_finite()));
    }
    // The other kind in the cache is an error, not a response.
    let ov = Cached::Overview("{}".into());
    assert!(li.render(&ov, &key, Groups::Top, None).is_err());
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// A fresh cache dir (not yet created: the cache makes it) under a temp dir
/// that is removed when the returned guard drops. Keep the guard alive for
/// the whole test.
fn temp_cache_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let guard = tempfile::TempDir::new().unwrap();
    let dir = guard.path().join("layouts");
    (guard, dir)
}

#[test]
fn the_disk_cache_is_private_and_replaces_a_corrupt_file() {
    let (_guard, dir) = temp_cache_dir();
    let input = LayoutInput::Force(sample(20));
    let key = input.digest();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    assert_eq!(c.dir(), Some(dir.as_path()));
    let mut runs = 0;
    let first = c
        .fill(&key, || {
            runs += 1;
            input.run(&key)
        })
        .unwrap();
    let file = dir.join(format!("{key}.bin"));
    assert!(file.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
    }
    // A fresh cache (a new process) reads it back without computing.
    let c2 = cache::LayoutCache::new(Some(dir.clone()), true);
    let again = c2.fill(&key, || unreachable!("served from disk")).unwrap();
    assert_eq!(*first, *again);
    // A corrupt file is ignored, recomputed and replaced.
    std::fs::write(&file, b"EDLC garbage").unwrap();
    let c3 = cache::LayoutCache::new(Some(dir.clone()), true);
    let fixed = c3
        .fill(&key, || {
            runs += 1;
            input.run(&key)
        })
        .unwrap();
    assert_eq!(*fixed, *first);
    assert_eq!(runs, 2);
    // A file naming another digest is not served for this one either.
    let other = "0".repeat(64);
    std::fs::copy(&file, dir.join(format!("{other}.bin"))).unwrap();
    let c4 = cache::LayoutCache::new(Some(dir.clone()), true);
    let mut ran = false;
    c4.fill(&other, || {
        ran = true;
        Ok(Cached::Overview("null".into()))
    })
    .unwrap();
    assert!(ran);
    // A symlink at a cache file's path is never followed.
    #[cfg(unix)]
    {
        let target = dir.join("target.bin");
        std::fs::copy(&file, &target).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        let c5 = cache::LayoutCache::new(Some(dir.clone()), true);
        let mut ran = false;
        c5.fill(&key, || {
            ran = true;
            input.run(&key)
        })
        .unwrap();
        assert!(ran, "a symlinked cache file is a miss");
    }
    // A file of the older format is deleted by the next prune.
    std::fs::write(dir.join(format!("{key}.json")), b"{}").unwrap();
    cache::prune_for_test(&dir, u64::MAX);
    assert!(!dir.join(format!("{key}.json")).exists());
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn the_disk_cache_evicts_the_oldest_past_its_cap() {
    let (_guard, dir) = temp_cache_dir();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    for k in 0..6 {
        let key = format!("{k:064x}");
        c.fill(&key, || Ok(Cached::Overview("x".repeat(1000))))
            .unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(600 - k * 60);
        std::fs::File::options()
            .append(true)
            .open(dir.join(format!("{key}.bin")))
            .unwrap()
            .set_modified(past)
            .unwrap();
    }
    cache::prune_for_test(&dir, 3000);
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(&format!("{:064x}.bin", 5)));
    assert!(left.contains(&format!("{:064x}.bin", 4)));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn prune_touches_only_the_caches_own_file_names() {
    use cache::{cache_file_kind, FileKind};
    let key = "a".repeat(64);
    assert_eq!(cache_file_kind(&format!("{key}.bin")), Some(FileKind::Bin));
    assert_eq!(
        cache_file_kind(&format!("{key}.json")),
        Some(FileKind::OldJson)
    );
    assert_eq!(
        cache_file_kind(&format!(".{key}.12.3.tmp")),
        Some(FileKind::Tmp)
    );
    for other in [
        "notes.json".to_string(),
        "photo.bin".to_string(),
        ".swap.tmp".to_string(),
        format!("{}.bin", "A".repeat(64)),
        format!(".{key}.12.tmp"),
        format!(".{key}.1.2.3.tmp"),
        format!("{key}.bin.bak"),
    ] {
        assert_eq!(cache_file_kind(&other), None, "{other}");
    }
    // A directory the user pointed the cache at keeps their files, however
    // old, under any cap.
    let (_guard, dir) = temp_cache_dir();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    assert!(c.dir().is_some());
    let theirs = ["notes.json", "photo.bin", "editor.tmp"];
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for f in theirs {
        std::fs::write(dir.join(f), vec![0u8; 5000]).unwrap();
        std::fs::File::options()
            .append(true)
            .open(dir.join(f))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    cache::prune_for_test(&dir, 0);
    for f in theirs {
        assert!(dir.join(f).exists(), "{f} was deleted");
    }
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_corrupt_overview_on_disk_is_a_miss() {
    let (_guard, dir) = temp_cache_dir();
    let key = "b".repeat(64);
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    c.fill(&key, || Ok(Cached::Overview("{\"v\":3,".into())))
        .unwrap();
    // A fresh process (an empty memory tier) reads the file: invalid JSON
    // is never spliced into a response, so it is computed again.
    let c2 = cache::LayoutCache::new(Some(dir.clone()), true);
    let mut ran = false;
    let v = c2
        .fill(&key, || {
            ran = true;
            Ok(Cached::Overview("{\"v\":3}".into()))
        })
        .unwrap();
    assert!(ran, "a corrupt overview file is a miss");
    assert_eq!(*v, Cached::Overview("{\"v\":3}".into()));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn one_computation_per_key_and_a_bounded_queue() {
    let c = cache::LayoutCache::new(None, true);
    let key = |k: usize| format!("{k:064x}");
    let cache::Flight::Lead(a) = c.begin(&key(0)) else {
        panic!("the first request leads");
    };
    let mut rx = a.subscribe();
    assert!(
        matches!(c.begin(&key(0)), cache::Flight::Wait(_)),
        "the same key waits"
    );
    // Every other key leads too, computing or queued, up to the bound.
    let mut leads = Vec::new();
    for k in 1..cache::MAX_COMPUTING + cache::MAX_QUEUED {
        match c.begin(&key(k)) {
            cache::Flight::Lead(l) => leads.push(l),
            _ => panic!("key {k} is within the queue"),
        }
    }
    assert!(
        matches!(c.begin(&key(1000)), cache::Flight::Full),
        "past MAX_COMPUTING + MAX_QUEUED: refused"
    );
    // A request for a key in flight still joins it when the queue is full.
    assert!(matches!(c.begin(&key(1)), cache::Flight::Wait(_)));
    let v = c
        .fill(&key(0), || Ok(Cached::Overview("one".into())))
        .unwrap();
    c.land(a, Ok(v));
    assert_eq!(
        rx.borrow_and_update()
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|v| (**v).clone()),
        Some(Cached::Overview("one".into()))
    );
    // A landed lead frees its place; so does one dropped without landing
    // (its computation panicked), whose waiters see the channel close.
    assert!(matches!(c.begin(&key(1000)), cache::Flight::Lead(_)));
    let dropped = leads.pop().unwrap();
    let gone = dropped.subscribe();
    drop(dropped);
    assert!(gone.has_changed().is_err());
    // A failure reaches the waiters as an error.
    let l = leads.pop().unwrap();
    let mut failed = l.subscribe();
    c.land(l, Err("bad input".into()));
    assert!(matches!(&*failed.borrow_and_update(), Some(Err(why)) if &**why == "bad input"));
}

#[tokio::test]
async fn a_lead_waits_its_turn_for_a_computing_slot() {
    let c = cache::LayoutCache::new(None, true);
    let key = |k: usize| format!("{k:064x}");
    let leads: Vec<cache::Lead> = (0..=cache::MAX_COMPUTING)
        .map(|k| match c.begin(&key(k)) {
            cache::Flight::Lead(l) => l,
            _ => panic!("lead {k}"),
        })
        .collect();
    let mut held = Vec::new();
    for l in &leads[..cache::MAX_COMPUTING] {
        held.push(l.permit().await);
    }
    let last = &leads[cache::MAX_COMPUTING];
    let wait = tokio::time::timeout(std::time::Duration::from_millis(50), last.permit()).await;
    assert!(
        wait.is_err(),
        "every computing slot is taken: the next one queues"
    );
    held.pop();
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), last.permit()).await;
    assert!(got.is_ok(), "a freed slot goes to the queued lead");
}

#[test]
fn a_shared_cache_dir_is_refused() {
    let (_guard, dir) = temp_cache_dir();
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = cache::LayoutCache::new(Some(dir.clone()), true);
        assert!(c.dir().is_none(), "a 0755 cache dir is not used");
    }
    // Disabled: nothing on disk, nothing in memory.
    let off = cache::LayoutCache::new(Some(dir.join("off")), false);
    assert!(off.dir().is_none());
    assert!(!dir.join("off").exists());
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// The contract Grid's output keeps: one finite position per memory, no two
/// memories on one grid point, every relation a finite route of two or more
/// points from its source (to its target when it has one), one label per
/// memory with placed labels never overlapping, one line per scope starting
/// at a memory, and finite bounds holding every memory and route.
fn check_grid_contract(name: &str, input: &ForceInput, out: &grid::GridOutput) {
    let n = input.nodes.len();
    assert_eq!(out.pos.len(), 2 * n, "{name}");
    assert!(out.pos.iter().all(|v| v.is_finite()), "{name}: positions");
    let mut seen = std::collections::HashSet::new();
    for i in 0..n {
        let p = (out.pos[2 * i].to_bits(), out.pos[2 * i + 1].to_bits());
        assert!(seen.insert(p), "{name}: two memories at one point");
    }
    let at = |i: i64| [out.pos[2 * i as usize], out.pos[2 * i as usize + 1]];
    assert_eq!(out.routes.len(), input.edges.len(), "{name}");
    for (k, (r, e)) in out.routes.iter().zip(&input.edges).enumerate() {
        assert!(r.len() >= 2 && r.len() < 256, "{name}: route {k}");
        assert!(
            r.iter().flatten().all(|v| v.is_finite()),
            "{name}: route {k}"
        );
        assert_eq!(r[0], at(e.s), "{name}: route {k} source");
        if e.t >= 0 {
            assert_eq!(*r.last().unwrap(), at(e.t), "{name}: route {k} target");
        }
    }
    assert_eq!(out.labels.len(), n, "{name}");
    let placed: Vec<&labels::Label> = out.labels.iter().filter(|l| l.placed).collect();
    assert!(!placed.is_empty() || n == 0, "{name}: no label placed");
    for (a, la) in placed.iter().enumerate() {
        assert!(la.x.is_finite() && la.y.is_finite(), "{name}: label");
        for lb in &placed[a + 1..] {
            assert!(!overlaps(&la.rect, &lb.rect), "{name}: labels overlap");
        }
    }
    let keys: std::collections::HashSet<&str> =
        input.nodes.iter().map(|n| n.top_scope.as_str()).collect();
    assert_eq!(out.lines.len(), keys.len(), "{name}: one line per scope");
    for l in &out.lines {
        let first = l.pts.first().expect("a line has a point");
        assert!(
            (0..n).any(|i| [out.pos[2 * i], out.pos[2 * i + 1]] == *first),
            "{name}: line {:?} starts off its memories",
            l.key
        );
    }
    let [bx, by, bw, bh] = out.bounds;
    assert!([bx, by, bw, bh].iter().all(|v| v.is_finite()) && bw > 0.0 && bh > 0.0);
    let inside = |[x, y]: [f64; 2]| x >= bx && x <= bx + bw && y >= by && y <= by + bh;
    for i in 0..n {
        assert!(
            inside(at(i as i64)),
            "{name}: memory {i} outside the bounds"
        );
    }
    for r in &out.routes {
        assert!(
            r.iter().all(|p| inside(*p)),
            "{name}: a route leaves the bounds"
        );
    }
}

#[test]
fn grid_keeps_its_contract_and_is_deterministic() {
    let mut cases: Vec<(String, ForceInput)> = [1usize, 2, 7, 60, 400]
        .iter()
        .map(|&n| (format!("sample {n}"), sample(n)))
        .collect();
    for (name, text) in FIXTURES {
        let fx: Value = serde_json::from_str(text).unwrap();
        if fx["layout"] == "scope-force" {
            cases.push((name.to_string(), fixture_force(&fx)));
        }
    }
    for (name, input) in &cases {
        let out = grid::layout(input);
        check_grid_contract(name, input, &out);
        let li = LayoutInput::Grid(input.clone());
        let key = li.digest();
        let a = li.run(&key).unwrap();
        assert_eq!(a, li.run(&key).unwrap(), "{name}: deterministic");
        // Through the cache's binary form, the same text.
        let back = Cached::from_bytes(&a.to_bytes()).unwrap();
        let text = li.render(&a, &key, Groups::Top, None).unwrap();
        assert_eq!(li.render(&back, &key, Groups::Top, None).unwrap(), text);
        // The encoding is parallel to the response: route k's points follow
        // route_points, and pos/labels are per node.
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["name"], "grid");
        let pos = unb64_f64(v["pos"].as_str().unwrap());
        let bits = |a: &[f64]| a.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&pos), bits(&out.pos), "{name}");
        let counts = unb64(v["route_points"].as_str().unwrap());
        let flat = unb64_f64(v["routes"].as_str().unwrap());
        assert_eq!(counts.len(), input.edges.len());
        let mut at = 0;
        for (k, &m) in counts.iter().enumerate() {
            let m = m as usize;
            let want: Vec<f64> = out.routes[k].iter().flatten().copied().collect();
            assert_eq!(
                bits(&flat[at..at + 2 * m]),
                bits(&want),
                "{name}: route {k}"
            );
            at += 2 * m;
        }
        assert_eq!(at, flat.len());
        assert_eq!(
            unb64(v["label_flags"].as_str().unwrap()).len(),
            input.nodes.len()
        );
        assert_eq!(v["chains"].as_array().unwrap().len(), out.lines.len());
    }
}

#[test]
fn a_cache_file_with_a_damaged_payload_is_recomputed() {
    let (_guard, dir) = temp_cache_dir();
    let input = LayoutInput::Force(sample(20));
    let key = input.digest();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    let first = c.fill(&key, || input.run(&key)).unwrap();
    let file = dir.join(format!("{key}.bin"));
    let good = std::fs::read(&file).unwrap();
    // One low mantissa bit of the first position: still a finite number,
    // the same length, the same header; only the checksum can tell.
    let mut bad = good.clone();
    bad[4 + 4 + 64 + 1 + 8] ^= 1;
    std::fs::write(&file, &bad).unwrap();
    let c2 = cache::LayoutCache::new(Some(dir.clone()), true);
    let mut ran = false;
    let again = c2
        .fill(&key, || {
            ran = true;
            input.run(&key)
        })
        .unwrap();
    assert!(ran, "a damaged file is a miss");
    assert_eq!(*again, *first);
    assert_eq!(std::fs::read(&file).unwrap(), good, "the file is rewritten");
    // forget() drops it from memory and disk.
    c2.forget(&key);
    assert!(c2.get_mem(&key).is_none());
    assert!(!file.exists());
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_non_finite_core_never_reads_back() {
    let core = scope_force::layout_core(&sample(20));
    let ok = Cached::Force(Box::new(core.clone()));
    assert_eq!(Cached::from_bytes(&ok.to_bytes()), Some(ok));
    for damage in [
        |c: &mut ForceCore| c.x[0] = f64::NAN,
        |c: &mut ForceCore| c.r[1] = f64::INFINITY,
        |c: &mut ForceCore| c.name_size = f64::NAN,
        |c: &mut ForceCore| c.max[0] = f64::NEG_INFINITY,
        |c: &mut ForceCore| c.labels[0].w = f64::NAN,
    ] {
        let mut bad = core.clone();
        damage(&mut bad);
        assert!(!bad.is_finite());
        assert_eq!(
            Cached::from_bytes(&Cached::Force(Box::new(bad)).to_bytes()),
            None
        );
    }
    // An empty graph's bounds stay at their ±infinity start: still a layout.
    let empty = scope_force::layout_core(&sample(0));
    assert!(empty.x.is_empty() && empty.is_finite());
}

#[test]
fn a_pre_checksum_cache_file_is_replaced() {
    let (_guard, dir) = temp_cache_dir();
    let input = LayoutInput::Force(sample(20));
    let key = input.digest();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    let v = c.fill(&key, || input.run(&key)).unwrap();
    let file = dir.join(format!("{key}.bin"));
    // The same layout in the format before the checksum.
    let mut old = b"EDLC".to_vec();
    old.extend_from_slice(&LAYOUT_VERSION.to_le_bytes());
    old.extend_from_slice(key.as_bytes());
    old.extend_from_slice(&v.to_bytes());
    std::fs::write(&file, &old).unwrap();
    let c2 = cache::LayoutCache::new(Some(dir.clone()), true);
    let mut ran = false;
    c2.fill(&key, || {
        ran = true;
        input.run(&key)
    })
    .unwrap();
    assert!(ran, "an EDLC file is foreign");
    assert!(std::fs::read(&file).unwrap().starts_with(b"EDL2"));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_core_that_does_not_fit_the_graph_is_not_rendered() {
    let small = scope_force::layout_core(&sample(20));
    let input = LayoutInput::Force(sample(21));
    let key = input.digest();
    let err = input
        .render(&Cached::Force(Box::new(small)), &key, Groups::Top, None)
        .unwrap_err();
    assert!(err.contains("does not fit"), "{err}");
}

#[test]
fn the_overview_holds_its_contract_at_5000_scopes() {
    // E16: nothing caps a store's top-scope count. Past PACK_MAX the discs
    // sit on a spiral (where large ones may touch, so no overlap check
    // here), and the m × m affinity matrix is not built.
    let m = 5000;
    let input = overview::OverviewInput {
        nodes: (0..m)
            .map(|i| overview::OverviewNode {
                id: format!("scope-{i}"),
                count: (1 + 5000 / (i + 1)) as f64,
                full_len: 14,
                short_len: 7,
            })
            .collect(),
        edges: (1..m)
            .map(|i| overview::OverviewEdge {
                s: i - 1,
                t: i,
                n: (i % 9 + 1) as f64,
            })
            .collect(),
        aspect: 0.6,
        width: 1240.0,
    };
    let out = overview::layout(&input);
    let b = &out.bounds;
    assert_eq!(out.pos.len(), 2 * m);
    assert_eq!(out.r.len(), m);
    assert_eq!(out.labels.len(), m, "a label per scope");
    assert_eq!(out.routes.len(), input.edges.len());
    assert!([b.x, b.y, b.w, b.h].iter().all(|v| v.is_finite()));
    assert!(out.routes.iter().flatten().flatten().all(|v| v.is_finite()));
    assert!(out
        .labels
        .iter()
        .all(|l| l.x.is_finite() && l.y.is_finite()));
    for i in 0..m {
        let (x, y, r) = (out.pos[2 * i], out.pos[2 * i + 1], out.r[i]);
        assert!(x.is_finite() && y.is_finite() && r > 0.0, "disc {i}");
        assert!(
            x - r >= b.x - 1e-6 && x + r <= b.x + b.w + 1e-6,
            "disc {i} outside"
        );
        assert!(
            y - r >= b.y - 1e-6 && y + r <= b.y + b.h + 1e-6,
            "disc {i} outside"
        );
    }
}

/// The `LAYOUT_VERSION` the [`PINNED`] digests were recorded at.
const PINNED_AT: u32 = 4;

/// (case, SHA-256 of the rendered layout text with its cache key blanked).
/// Any change here means the layout's output changed: bump `LAYOUT_VERSION`
/// and update the digests and [`PINNED_AT`] together. The cache key is
/// blanked because it is salted with the crate version, which a release
/// changes without changing a single position.
const PINNED: &[(&str, &str)] = &[
    (
        "force-top/top",
        "80f03c38e6661e92fdaf2e3cad4e7dd5ca088acd08be41fd898e0919929a4bb6",
    ),
    (
        "force-top/sub",
        "a46d0c79c3c2c9bf981260aa3e4219950d4ea618f38252a1560036242aabf342",
    ),
    (
        "force-sub/top",
        "be2bc08835391e0fc5825e7139996237b76d081b31e39dd4abab67601101342e",
    ),
    (
        "force-sub/sub",
        "558a12b1e42e49f57059f252300d26ddfd1d10892660991d95380b9daffd7344",
    ),
    (
        "overview",
        "455867bdfa66dfb326496c8332723a02d9f957e5ec093fc16090d51ef89f4f42",
    ),
    (
        "grid/sample-60",
        "2230ef78b6f0f65f29f404878563f37f39dd1a3f69d7718d0bd2f5f18ed21857",
    ),
];

/// Every pinned case's rendered text: the three fixtures (each scope-force
/// one under both groupings, at the fixture's own width), and a generated
/// graph through `grid`.
fn pinned_cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();
    for (name, text) in FIXTURES {
        let fx: Value = serde_json::from_str(text).unwrap();
        match fx["layout"].as_str().unwrap() {
            "scope-force" => {
                let input = fixture_force(&fx);
                for (g, groups) in [("top", Groups::Top), ("sub", Groups::Sub)] {
                    let li = LayoutInput::Force(input.clone());
                    let key = li.digest();
                    let text = li.render(&li.run(&key).unwrap(), &key, groups, None);
                    cases.push((format!("{name}/{g}"), text.unwrap().replace(&key, "KEY")));
                }
            }
            "scope-overview" => {
                let li = LayoutInput::Overview(fixture_overview(&fx));
                let key = li.digest();
                let text = li.render(&li.run(&key).unwrap(), &key, Groups::Top, None);
                cases.push((name.to_string(), text.unwrap().replace(&key, "KEY")));
            }
            other => panic!("{name}: unknown layout {other}"),
        }
    }
    let li = LayoutInput::Grid(sample(60));
    let key = li.digest();
    let text = li.render(&li.run(&key).unwrap(), &key, Groups::Top, None);
    cases.push(("grid/sample-60".into(), text.unwrap().replace(&key, "KEY")));
    cases
}

#[test]
fn the_fixture_layouts_match_their_pinned_digests() {
    assert_eq!(
        PINNED_AT, LAYOUT_VERSION,
        "LAYOUT_VERSION changed: re-record PINNED and set PINNED_AT to it"
    );
    let actual: Vec<(String, String)> = pinned_cases()
        .into_iter()
        .map(|(name, text)| {
            let digest = Sha256::digest(text.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            (name, digest)
        })
        .collect();
    let table: String = actual
        .iter()
        .map(|(n, d)| format!("    (\"{n}\", \"{d}\"),\n"))
        .collect();
    assert_eq!(
        actual.len(),
        PINNED.len(),
        "PINNED does not list every case; the current digests are:\n{table}"
    );
    for ((name, digest), (pinned_name, pinned)) in actual.iter().zip(PINNED) {
        assert_eq!(name, pinned_name, "PINNED is out of order:\n{table}");
        assert_eq!(
            digest, pinned,
            "layout output changed for {name}: bump LAYOUT_VERSION and update PINNED \
             (current digests:\n{table})"
        );
    }
}

/// xorshift64*: reproducible bytes for the cache-decoder test.
struct Gen(u64);

impl Gen {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// A cache file is read back from a directory the user may point anywhere,
/// so its payload is untrusted. Random bytes, huge length prefixes and a
/// valid payload with any one byte flipped must each decode to nothing or to
/// a value that re-encodes to exactly those bytes, never panic, and never
/// allocate on a length the input cannot back.
#[test]
fn the_cache_decoder_rejects_generated_bytes() {
    let check = |bytes: &[u8], what: &str| {
        if let Some(v) = Cached::from_bytes(bytes) {
            assert_eq!(
                v.to_bytes(),
                bytes,
                "{what}: accepted bytes re-encode differently"
            );
        }
    };
    let mut g = Gen(0xdead_beef_f00d_0001);
    for case in 0..5000 {
        let len = (g.next() % 200) as usize;
        let mut bytes: Vec<u8> = (0..len).map(|_| g.next() as u8).collect();
        if let Some(first) = bytes.first_mut() {
            *first %= 4; // mostly a known tag, sometimes not
        }
        check(&bytes, &format!("random case {case}"));
    }
    // A length prefix claiming far more than the input holds.
    for tag in 0u8..3 {
        for n in [u64::MAX, u64::MAX / 8, 1 << 40, 4096] {
            let mut bytes = vec![tag];
            bytes.extend_from_slice(&n.to_le_bytes());
            bytes.extend_from_slice(&[0u8; 32]);
            assert_eq!(Cached::from_bytes(&bytes), None, "tag {tag}, length {n}");
        }
    }
    // Every single-byte flip of valid payloads of each kind.
    let force = LayoutInput::Force(sample(8));
    let key = force.digest();
    let grid = LayoutInput::Grid(sample(8));
    let gkey = grid.digest();
    for (what, valid) in [
        ("force", force.run(&key).unwrap()),
        ("grid", grid.run(&gkey).unwrap()),
        (
            "overview",
            Cached::Overview("{\"ids\":[\"a\"]}".to_string()),
        ),
    ] {
        let bytes = valid.to_bytes();
        assert_eq!(Cached::from_bytes(&bytes).as_ref(), Some(&valid), "{what}");
        for at in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[at] ^= 1 << (g.next() % 8);
            check(&flipped, &format!("{what} flipped at {at}"));
        }
        for cut in 0..bytes.len() {
            assert_eq!(
                Cached::from_bytes(&bytes[..cut]),
                None,
                "{what} cut at {cut}"
            );
        }
    }
}
