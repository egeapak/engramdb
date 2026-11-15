//! Races between separate `engramdb` processes on one store.
//!
//! The in-process tests in `engram-storage` race tasks; the race that reaches
//! users is between processes (every session's hooks after an upgrade, two
//! terminals), where the advisory `flock` is what serializes them.

use crate::helpers::*;
use regex::Regex;
use tempfile::TempDir;

fn schema_version(manifest: &str) -> String {
    Regex::new(r#"(?m)^schema_version\s*=\s*"([^"]*)""#)
        .unwrap()
        .captures(manifest)
        .expect("manifest has a schema_version")[1]
        .to_string()
}

/// Two `list` processes opening a store whose manifest is behind both
/// succeed, list the same memories, and leave the store stamped current.
#[test]
fn two_processes_opening_a_stale_store_agree() {
    let tmp = TempDir::new().unwrap();
    init_store(tmp.path());
    let mut ids = seed_store(tmp.path());
    ids.sort();

    let manifest_path = tmp.path().join(".engramdb").join("manifest.toml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    let current = schema_version(&manifest);
    let stale = Regex::new(r#"(?m)^schema_version\s*=\s*"[^"]*""#)
        .unwrap()
        .replace(&manifest, r#"schema_version = "0.6.0""#)
        .into_owned();
    std::fs::write(&manifest_path, stale).unwrap();
    assert_eq!(
        schema_version(&std::fs::read_to_string(&manifest_path).unwrap()),
        "0.6.0"
    );

    let children: Vec<_> = (0..2)
        .map(|_| {
            std_cmd()
                .args(["--dir", tmp.path().to_str().unwrap(), "--json", "list"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let id_re =
        Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "a racing open failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut listed: Vec<String> = id_re
            .find_iter(&stdout)
            .map(|m| m.as_str().to_string())
            .collect();
        listed.sort();
        listed.dedup();
        assert_eq!(listed, ids, "a racing open listed different memories");
    }

    assert_eq!(
        schema_version(&std::fs::read_to_string(&manifest_path).unwrap()),
        current,
        "the store must end stamped current"
    );
}

/// Checks that failed outright (not a warning or a note), from every section
/// and subsection of `doctor --json`.
fn failed_checks(doctor: &serde_json::Value) -> Vec<String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(checks) = v.get("checks").and_then(|c| c.as_array()) {
            for c in checks {
                let status = c
                    .get("status")
                    .and_then(|s| s.as_str())
                    .map(str::to_ascii_lowercase);
                let failed = match status.as_deref() {
                    Some("fail") => true,
                    Some(_) => false,
                    None => c.get("passed") == Some(&serde_json::Value::Bool(false)),
                };
                if failed {
                    out.push(c.to_string());
                }
            }
        }
        for key in ["sections", "subsections"] {
            if let Some(children) = v.get(key).and_then(|s| s.as_array()) {
                for child in children {
                    walk(child, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(doctor, &mut out);
    out
}

/// Eight `add` processes started together on one store all persist: the
/// per-project `flock` serializes their index writes, every file lands, no
/// temp file is left behind, and `doctor` finds the index in step.
#[test]
fn concurrent_adds_from_separate_processes_all_persist() {
    let tmp = TempDir::new().unwrap();
    init_store(tmp.path());
    // Keyword titles: no child may need a model.
    let config = tmp.path().join(".engramdb").join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    assert!(!text.contains("[title]"), "init now writes [title]: {text}");
    text.push_str("\n[title]\nstrategy = \"keyword\"\n");
    std::fs::write(&config, text).unwrap();

    let dir = tmp.path().to_str().unwrap().to_string();
    let children: Vec<_> = (0..8)
        .map(|i| {
            std_cmd()
                .args([
                    "--dir",
                    &dir,
                    "add",
                    "-t",
                    "decision",
                    "-s",
                    &format!("Concurrent decision number {i}"),
                    "-c",
                    &format!("Written by child {i} while seven others write too."),
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut ids = Vec::new();
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "add child {i} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        ids.push(extract_id(&out.stdout));
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8, "every child must create its own memory");

    let out = cmd()
        .args(["--dir", &dir, "--json", "list"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id_re =
        Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
    let mut listed: Vec<String> = id_re
        .find_iter(&String::from_utf8_lossy(&out.stdout))
        .map(|m| m.as_str().to_string())
        .collect();
    listed.sort();
    listed.dedup();
    assert_eq!(listed, ids, "the index must list every memory written");

    let memories = tmp.path().join(".engramdb").join("memories");
    let names: Vec<String> = std::fs::read_dir(&memories)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.ends_with(".md")).count(),
        8,
        "{names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains(".tmp")),
        "a temp file was left behind: {names:?}"
    );

    let out = cmd()
        .args(["--dir", &dir, "--json", "doctor"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doctor: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(failed_checks(&doctor), Vec::<String>::new());
    assert!(
        !stdout.contains("out of step"),
        "doctor reports drift: {stdout}"
    );
}
