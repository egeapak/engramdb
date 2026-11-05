//! Unit tests of the layout ports: bit-parity with the JS on the committed
//! golden fixtures (`fixtures/`, written by `node bench/golden.mjs`), the
//! JS hash pinned to the version, the helpers' JS semantics, the output
//! contract, determinism and the cache. `bench/server-layout.mjs` checks
//! parity on real stores against a running dashboard.

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

/// Editing a layout JS file (or the input builder or decoder) changes
/// `LAYOUT_JS_HASH`, which fails this test until the Rust port is checked
/// against it (`node bench/golden.mjs`, then the golden test), the version
/// bumped and the pair below re-pinned.
#[test]
fn the_layout_js_is_pinned_to_the_layout_version() {
    assert_eq!(
        (LAYOUT_VERSION, LAYOUT_JS_HASH),
        (2, "46ee3d62895a7278"),
        "a layout JS file changed: port the change to src/layout/, regenerate \
         the golden fixtures (node bench/golden.mjs), bump LAYOUT_VERSION and \
         re-pin this pair"
    );
    // The runtime hash (`--assets-dir`) is build.rs's over the same bytes.
    let embedded = js_hash(|p| crate::assets::find(p).map(|a| a.bytes.to_vec()));
    assert_eq!(embedded.as_deref(), Some(LAYOUT_JS_HASH));
    // Every file it hashes is embedded, and every layout module is hashed.
    for f in LAYOUT_JS_FILES {
        assert!(crate::assets::find(f).is_some(), "{f}");
    }
    for f in [
        "js/graph/scope-force.js",
        "js/graph/scope-overview.js",
        "js/graph/labels.js",
        "js/graph/radius.js",
        "js/graph/opened.js",
        "js/graph/input.js",
    ] {
        assert!(LAYOUT_JS_FILES.contains(&f), "{f} is not hashed");
    }
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

#[test]
fn the_encoding_round_trips() {
    let input = sample(40);
    let out = scope_force::layout(&input);
    let v = encode_force(&out, &input, "k", Groups::Sub);
    assert_eq!(v["v"], LAYOUT_VERSION);
    assert_eq!(v["n"], 40);
    assert_eq!(v["groups"], "sub");
    assert_eq!(v["check"], input_check(&input));
    let pos = unb64_f64(v["pos"].as_str().unwrap());
    assert_eq!(pos, out.pos);
    let spots = unb64(v["labels"].as_str().unwrap());
    assert_eq!(spots.len(), 40);
    for (s, l) in spots.iter().zip(&out.labels) {
        assert_eq!(*s == 255, !l.placed);
    }
    // Two stubs, one end (two doubles) each.
    assert_eq!(unb64(v["stubs"].as_str().unwrap()).len(), 2 * 2 * 8);
}

// ---------------------------------------------------------------------------
// Golden fixtures: the JS layouts' output, bit for bit
// ---------------------------------------------------------------------------

/// The fixtures `node bench/golden.mjs` writes: a response's fields, the
/// request, the input's check and the JS layout's output (doubles as
/// base64 little-endian, so no JSON parse can round them).
const GOLDEN: &[(&str, &str)] = &[
    ("force-top", include_str!("fixtures/force-top.json")),
    ("force-sub", include_str!("fixtures/force-sub.json")),
    ("overview", include_str!("fixtures/overview.json")),
];

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn check_force(name: &str, fx: &Value) {
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
    // Exact: k / 20 is the double the page's Math.round(a * 20) / 20 gives.
    let aspect = fx["aspect20"].as_f64().unwrap() / 20.0;
    let width = fx["width"].as_f64().unwrap();
    let input = build_force_input(&nodes, &edges, groups, aspect, width);
    assert_eq!(
        u64::from(input_check(&input)),
        fx["check"].as_u64().unwrap(),
        "{name}: input check (the input builders differ)"
    );
    let out = scope_force::layout(&input);
    let want = &fx["out"];
    let f = |k: &str| unb64_f64(want[k].as_str().unwrap());
    assert_eq!(bits(&out.pos), bits(&f("pos")), "{name}: pos");
    let routes: Vec<f64> = out.routes.iter().flatten().copied().collect();
    assert_eq!(bits(&routes), bits(&f("routes")), "{name}: routes");
    let lab: Vec<f64> = out.labels.iter().flat_map(|l| [l.x, l.y, l.w]).collect();
    assert_eq!(bits(&lab), bits(&f("labels")), "{name}: labels");
    let anchors: String = out
        .labels
        .iter()
        .map(|l| {
            format!(
                "{}{}",
                &l.anchor.as_str()[..1],
                if l.placed { "+" } else { "-" }
            )
        })
        .collect();
    assert_eq!(
        anchors,
        want["anchors"].as_str().unwrap(),
        "{name}: anchors"
    );
    assert_eq!(bits(&out.bounds), bits(&f("bounds")), "{name}: bounds");
    assert_eq!(
        out.region_r.to_bits(),
        f("region_r")[0].to_bits(),
        "{name}: region_r"
    );
    let names: Vec<f64> = out
        .scopes
        .iter()
        .flat_map(|s| [s.x, s.y, s.w, s.size])
        .collect();
    assert_eq!(bits(&names), bits(&f("scopes")), "{name}: scope names");
    let flags: Vec<String> = out
        .scopes
        .iter()
        .map(|s| format!("{}:{}{}", s.key, u8::from(s.placed), u8::from(s.backed)))
        .collect();
    let want_flags: Vec<String> = want["scope_flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(flags, want_flags, "{name}: scope name flags");
}

fn check_overview(name: &str, fx: &Value) {
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
    let aspect = fx["aspect20"].as_f64().unwrap() / 20.0;
    let input = build_overview_input(&scopes, &rows, aspect, fx["width"].as_f64().unwrap());
    let out = overview::layout(&input);
    let want = &fx["out"];
    let f = |k: &str| unb64_f64(want[k].as_str().unwrap());
    assert_eq!(bits(&out.pos), bits(&f("pos")), "{name}: pos");
    assert_eq!(bits(&out.r), bits(&f("r")), "{name}: r");
    let routes: Vec<f64> = out.routes.iter().flatten().flatten().copied().collect();
    assert_eq!(bits(&routes), bits(&f("routes")), "{name}: routes");
    let lab: Vec<f64> = out.labels.iter().flat_map(|l| [l.x, l.y, l.w]).collect();
    assert_eq!(bits(&lab), bits(&f("labels")), "{name}: labels");
    let flags: String = out
        .labels
        .iter()
        .map(|l| {
            format!(
                "{}{}{}{}",
                &l.anchor[..1],
                u8::from(l.placed),
                u8::from(l.short),
                u8::from(l.forced)
            )
        })
        .collect();
    assert_eq!(
        flags,
        want["label_flags"].as_str().unwrap(),
        "{name}: label flags"
    );
    let b = &out.bounds;
    assert_eq!(
        bits(&[b.x, b.y, b.w, b.h]),
        bits(&f("bounds")),
        "{name}: bounds"
    );
}

#[test]
fn the_ports_match_the_js_on_the_golden_fixtures() {
    for (name, text) in GOLDEN {
        let fx: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            fx["v"], LAYOUT_VERSION,
            "{name}: regenerate with node bench/golden.mjs"
        );
        match fx["layout"].as_str().unwrap() {
            "scope-force" => check_force(name, &fx),
            "scope-overview" => check_overview(name, &fx),
            other => panic!("{name}: unknown layout {other}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

fn temp_cache_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "engram-layout-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dir.join("layouts")
}

#[test]
fn the_disk_cache_is_private_and_replaces_a_corrupt_file() {
    let dir = temp_cache_dir();
    let input = LayoutInput::Force(sample(20));
    let key = input.digest();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    assert_eq!(c.dir(), Some(dir.as_path()));
    let mut runs = 0;
    let first = c.fill(&key, || {
        runs += 1;
        input.run(&key)
    });
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
    let again = c2.fill(&key, || unreachable!("served from disk"));
    assert_eq!(*first, *again);
    // A corrupt file is ignored, recomputed and replaced.
    std::fs::write(&file, b"EDLC garbage").unwrap();
    let c3 = cache::LayoutCache::new(Some(dir.clone()), true);
    let fixed = c3.fill(&key, || {
        runs += 1;
        input.run(&key)
    });
    assert_eq!(*fixed, *first);
    assert_eq!(runs, 2);
    // A file naming another digest is not served for this one either.
    let other = "0".repeat(64);
    std::fs::copy(&file, dir.join(format!("{other}.bin"))).unwrap();
    let c4 = cache::LayoutCache::new(Some(dir.clone()), true);
    let mut ran = false;
    c4.fill(&other, || {
        ran = true;
        Cached::Overview("null".into())
    });
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
        });
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
    let dir = temp_cache_dir();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    for k in 0..6 {
        let key = format!("{k:064x}");
        c.fill(&key, || Cached::Overview("x".repeat(1000)));
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
fn one_computation_per_key_and_a_bounded_number_at_once() {
    let c = cache::LayoutCache::new(None, true);
    let key = |k: u32| format!("{k:064x}");
    let cache::Flight::Lead(a) = c.begin(&key(1)) else {
        panic!("the first request leads");
    };
    let mut rx = a.subscribe();
    assert!(
        matches!(c.begin(&key(1)), cache::Flight::Wait(_)),
        "the same key waits"
    );
    let cache::Flight::Lead(b) = c.begin(&key(2)) else {
        panic!("a second key leads");
    };
    assert!(
        matches!(c.begin(&key(3)), cache::Flight::Busy),
        "past MAX_COMPUTING: none"
    );
    let v = c.fill(&key(1), || Cached::Overview("one".into()));
    c.land(a, v);
    assert_eq!(
        rx.borrow_and_update().as_deref(),
        Some(&Cached::Overview("one".into()))
    );
    // A lead dropped without landing (its computation panicked) frees the
    // key and its permit.
    drop(b);
    assert!(matches!(c.begin(&key(2)), cache::Flight::Lead(_)));
    assert!(matches!(c.begin(&key(3)), cache::Flight::Lead(_)));
}

#[test]
fn a_shared_cache_dir_is_refused() {
    let dir = temp_cache_dir();
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
