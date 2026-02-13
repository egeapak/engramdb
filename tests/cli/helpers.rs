use regex::Regex;
use std::path::Path;

/// Create a new Command pointing to the `engramdb` binary.
#[allow(deprecated)]
pub fn cmd() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("engramdb").expect("binary engramdb not found")
}

/// Initialize a store at the given directory with `--no-embeddings`.
pub fn init_store(dir: &Path) {
    cmd()
        .args(["--dir", dir.to_str().unwrap(), "init", "--no-embeddings"])
        .assert()
        .success();
}

/// Add a memory and return its UUID.
pub fn add_memory(dir: &Path, type_: &str, summary: &str, content: &str) -> String {
    let output = cmd()
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "add",
            "-t",
            type_,
            "-s",
            summary,
            "-c",
            content,
        ])
        .output()
        .expect("failed to run add command");

    assert!(
        output.status.success(),
        "add command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    extract_id(&output.stdout)
}

/// Add a memory with arbitrary extra args and return its UUID.
pub fn add_memory_with_args(dir: &Path, args: &[&str]) -> String {
    let mut all_args = vec!["--dir", dir.to_str().unwrap(), "add"];
    all_args.extend_from_slice(args);

    let output = cmd()
        .args(&all_args)
        .output()
        .expect("failed to run add command");

    assert!(
        output.status.success(),
        "add command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    extract_id(&output.stdout)
}

/// Extract a UUID from command output.
///
/// Looks for UUIDv7 patterns (hex with dashes) in the output.
pub fn extract_id(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let re = Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
        .expect("invalid regex");
    re.find(&text)
        .unwrap_or_else(|| panic!("no UUID found in output: {}", text))
        .as_str()
        .to_string()
}

/// Seed a store with a few memories and return their IDs.
pub fn seed_store(dir: &Path) -> Vec<String> {
    let id1 = add_memory(
        dir,
        "decision",
        "Use Rust for backend",
        "We decided to use Rust",
    );
    let id2 = add_memory(
        dir,
        "convention",
        "Use snake_case",
        "All variables should use snake_case",
    );
    let id3 = add_memory(
        dir,
        "hazard",
        "Avoid unwrap in production",
        "Never use unwrap in production code",
    );
    vec![id1, id2, id3]
}
