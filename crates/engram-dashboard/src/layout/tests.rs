//! Unit tests of the layout ports. Bit-parity with the JS on real stores is
//! `bench/server-layout.mjs`'s job (it needs Node); these pin the helpers'
//! JS semantics, the output contract, determinism and the cache.

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
}

#[test]
fn the_encoding_round_trips() {
    let input = sample(40);
    let out = scope_force::layout(&input);
    let v = encode_force(&out, &input, "k");
    assert_eq!(v["v"], LAYOUT_VERSION);
    assert_eq!(v["n"], 40);
    let unb64 = |s: &str| -> Vec<u8> {
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
    };
    let pos: Vec<f64> = unb64(v["pos"].as_str().unwrap())
        .chunks(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(pos, out.pos);
    let spots = unb64(v["labels"].as_str().unwrap());
    assert_eq!(spots.len(), 40);
    for (s, l) in spots.iter().zip(&out.labels) {
        assert_eq!(*s == 255, !l.placed);
    }
    // Two stubs, one end (two doubles) each.
    assert_eq!(unb64(v["stubs"].as_str().unwrap()).len(), 2 * 2 * 8);
}

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
    let first = c.get_or_compute(&key, || {
        runs += 1;
        input.run(&key)
    });
    let file = dir.join(format!("{key}.json"));
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
    let again = c2.get_or_compute(&key, || unreachable!("served from disk"));
    assert_eq!(*first, *again);
    // A corrupt file is ignored, recomputed and replaced.
    std::fs::write(&file, b"{not json").unwrap();
    let c3 = cache::LayoutCache::new(Some(dir.clone()), true);
    let fixed = c3.get_or_compute(&key, || {
        runs += 1;
        input.run(&key)
    });
    assert_eq!(*fixed, *first);
    assert_eq!(runs, 2);
    let back: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(back["key"], key.as_str());
    // A file naming another digest is not served for this one either.
    std::fs::copy(&file, dir.join(format!("{}.json", "0".repeat(64)))).unwrap();
    let c4 = cache::LayoutCache::new(Some(dir.clone()), true);
    let mut ran = false;
    c4.get_or_compute(&"0".repeat(64), || {
        ran = true;
        "null".to_string()
    });
    assert!(ran);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn the_disk_cache_evicts_the_oldest_past_its_cap() {
    let dir = temp_cache_dir();
    let c = cache::LayoutCache::new(Some(dir.clone()), true);
    for k in 0..6 {
        let key = format!("{k:064x}");
        c.get_or_compute(&key, || {
            serde_json::json!({ "v": LAYOUT_VERSION, "key": key, "pad": "x".repeat(1000) })
                .to_string()
        });
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(600 - k * 60);
        std::fs::File::options()
            .append(true)
            .open(dir.join(format!("{key}.json")))
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
    assert!(left.contains(&format!("{:064x}.json", 5)));
    assert!(left.contains(&format!("{:064x}.json", 4)));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
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
