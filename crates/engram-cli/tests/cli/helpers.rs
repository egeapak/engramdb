use regex::Regex;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tempfile::TempDir;

struct TestDirs {
    registry_dir: TempDir,
    data_dir: TempDir,
    config_dir: TempDir,
    home_dir: TempDir,
    runtime_dir: TempDir,
}

fn test_dirs() -> &'static TestDirs {
    static DIRS: OnceLock<TestDirs> = OnceLock::new();
    DIRS.get_or_init(|| TestDirs {
        registry_dir: TempDir::new().unwrap(),
        data_dir: TempDir::new().unwrap(),
        config_dir: TempDir::new().unwrap(),
        home_dir: TempDir::new().unwrap(),
        runtime_dir: private_tempdir(),
    })
}

/// A temp dir with mode 0700: the daemon folder (the socket's parent) must be
/// private, or `doctor` reports it refused, and `TempDir::new` follows the
/// umask.
pub fn private_tempdir() -> TempDir {
    let dir = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

/// The real model cache, resolved in this (test) process before any child's
/// `HOME` moves: `ENGRAMDB_MODEL_CACHE_DIR` when set, else
/// `<cache dir>/engramdb/models`. The tests that need the default model keep
/// finding it although `HOME` points at a temp dir.
fn real_model_cache() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ENGRAMDB_MODEL_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::cache_dir().map(|c| c.join("engramdb").join("models"))
}

/// The environment every spawned `engramdb` gets on top of its data, config
/// and registry dirs, so a black-box test never reaches the developer's
/// machine: no running daemon (in-process, and a socket under `runtime` that
/// nothing binds — a client would otherwise send `Shutdown` to an older
/// daemon), no real home or XDG dirs, no `~/.claude`, no inherited session
/// ids or editor. `None` removes the variable. The one thing kept real is the
/// model cache (see [`real_model_cache`]).
///
/// Shared by [`cmd`], the snapshot fixture and the dashboard tests, so a new
/// variable is added in one place; a caller's later `.env()` wins.
pub fn isolation_env(home: &Path, runtime: &Path) -> Vec<(&'static str, Option<OsString>)> {
    let mut env: Vec<(&'static str, Option<OsString>)> = vec![
        ("ENGRAMDB_IN_PROCESS", Some("1".into())),
        (
            "ENGRAMDB_DAEMON_SOCKET",
            Some(runtime.join("d.sock").into_os_string()),
        ),
        ("XDG_RUNTIME_DIR", Some(runtime.as_os_str().to_owned())),
        ("HOME", Some(home.as_os_str().to_owned())),
        (
            "XDG_CONFIG_HOME",
            Some(home.join("config").into_os_string()),
        ),
        ("XDG_CACHE_HOME", Some(home.join("cache").into_os_string())),
        ("XDG_DATA_HOME", Some(home.join("data").into_os_string())),
        (
            "CLAUDE_CONFIG_DIR",
            Some(home.join(".claude").into_os_string()),
        ),
        ("CLAUDE_SESSION_ID", None),
        ("MCP_SESSION_ID", None),
        ("EDITOR", None),
        ("VISUAL", None),
    ];
    if let Some(cache) = real_model_cache() {
        env.push(("ENGRAMDB_MODEL_CACHE_DIR", Some(cache.into_os_string())));
    }
    env
}

/// Apply [`isolation_env`] to an `assert_cmd` command.
pub fn isolate(c: &mut assert_cmd::Command, home: &Path, runtime: &Path) {
    for (k, v) in isolation_env(home, runtime) {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
}

/// The runtime dir [`cmd`]'s children use (their daemon socket's folder).
pub fn runtime_dir() -> &'static Path {
    test_dirs().runtime_dir.path()
}

/// Create a new Command pointing to the `engramdb` binary.
///
/// Sets `ENGRAMDB_REGISTRY_PATH`, `ENGRAMDB_DATA_DIR` and
/// `ENGRAMDB_CONFIG_DIR` to shared temp dirs and applies [`isolation_env`],
/// so integration tests never pollute the user's global state or talk to the
/// user's daemon.
#[allow(deprecated)]
pub fn cmd() -> assert_cmd::Command {
    let dirs = test_dirs();
    let mut c = assert_cmd::Command::cargo_bin("engramdb").expect("binary engramdb not found");
    c.env(
        "ENGRAMDB_REGISTRY_PATH",
        dirs.registry_dir.path().join("registry.json"),
    );
    c.env("ENGRAMDB_DATA_DIR", dirs.data_dir.path());
    c.env("ENGRAMDB_CONFIG_DIR", dirs.config_dir.path());
    isolate(&mut c, dirs.home_dir.path(), dirs.runtime_dir.path());
    c
}

/// The shared `ENGRAMDB_DATA_DIR` these tests run against.
///
/// Transcript archives land under `projects/<root-project-id>/transcripts/`,
/// and that id is a hash of a temp path, so tests locate an archive by
/// searching rather than recomputing it.
pub fn data_dir() -> &'static Path {
    test_dirs().data_dir.path()
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
