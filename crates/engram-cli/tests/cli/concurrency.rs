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
