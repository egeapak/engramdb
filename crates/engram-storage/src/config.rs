//! Configuration loading with defaults.
//!
//! This module provides the `load_config()` function to load EngramDB
//! configuration from config.toml in the project directory. If the file
//! doesn't exist, returns default configuration values.
//!
//! Configuration is defined in [`engram_types::EngramConfig`] and includes
//! scoring weights, retrieval thresholds, scope bonuses, and trust weights.
//!
//! The global, per-user config file ([`engram_types::GlobalConfig`], at
//! [`crate::paths::global_config_path`]) is loaded by [`load_global_config`] /
//! [`load_global_config_or_default`]. It holds the shared daemon's `[daemon]`
//! section; a project config that still sets `[daemon]` keys loads, but the
//! keys are ignored and reported once per path.

use super::error::Result;
use engram_types::{EngramConfig, GlobalConfig};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Load configuration from config.toml, or return defaults if file doesn't exist
///
/// A config that parses but fails [`EngramConfig::validate`] (weights not
/// summing to 1.0, out-of-range timeouts, …) is returned as-is with a loud
/// warning: runtime behavior stays permissive (score math clamps; `doctor`
/// is the diagnostic surface), but the problem is no longer invisible.
pub async fn load_config(config_path: &Path) -> Result<EngramConfig> {
    if !config_path.exists() {
        return Ok(EngramConfig::default());
    }

    let content = tokio::fs::read_to_string(config_path).await?;
    let config: EngramConfig = toml::from_str(&content)?;
    warn_legacy_daemon_keys(config_path, &config);
    if let Err(e) = config.validate() {
        warn_once(
            config_path,
            WarnKind::Invalid,
            &format!(
                "config file {} is invalid ({e}); continuing with the values as written — \
                 run `engramdb doctor` for details",
                config_path.display()
            ),
        );
    }
    Ok(config)
}

/// Load configuration, falling back to defaults on ANY failure — loudly.
///
/// This is the shared replacement for the `load_config(...).unwrap_or_default()`
/// pattern: front-ends that must never fail because of a bad config (the MCP
/// server, provider resolution, store open) still get defaults, but a config
/// file that fails to parse is reported once per path per process instead of
/// being silently ignored wholesale. A partial section (e.g. `[nli]` with
/// required fields missing) fails the whole-file parse, so without the
/// warning a user's entire config — including valid sections — vanished
/// without a trace.
pub async fn load_config_or_default(config_path: &Path) -> EngramConfig {
    match load_config(config_path).await {
        Ok(config) => config,
        Err(e) => {
            warn_once(
                config_path,
                WarnKind::Unreadable,
                &format!(
                    "ignoring config file {}: {e}; ALL settings in it are falling back to \
                     defaults — fix the file (see `engramdb doctor`) to restore them",
                    config_path.display()
                ),
            );
            EngramConfig::default()
        }
    }
}

/// The message for a project config that still sets `[daemon]` keys, or
/// `None` when it sets none. Shared by the loader's warning and `doctor`, so
/// both name the same keys and the same replacement path.
pub fn legacy_daemon_message(config_path: &Path, config: &EngramConfig) -> Option<String> {
    let mut keys = config.legacy_daemon_keys();
    if keys.is_empty() {
        return None;
    }
    keys.sort();
    let keys = keys
        .iter()
        .map(|k| format!("daemon.{k}"))
        .collect::<Vec<_>>()
        .join(", ");
    let global = crate::paths::global_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "the global config file".to_string());
    Some(format!(
        "{} sets {keys}, which is ignored: the daemon is shared by every project, \
         so [daemon] is read only from the global config {global} — move the \
         keys there and delete them from the project config",
        config_path.display()
    ))
}

/// Warn once per path when a project config still sets `[daemon]` keys.
fn warn_legacy_daemon_keys(config_path: &Path, config: &EngramConfig) {
    if let Some(message) = legacy_daemon_message(config_path, config) {
        warn_once(config_path, WarnKind::LegacyDaemon, &message);
    }
}

/// Load the global, per-user config file at `config_path`, or defaults when
/// it does not exist.
///
/// Same contract as [`load_config`]: a parse error is an error, a file that
/// parses but fails [`GlobalConfig::validate`] is returned as written with a
/// warning. Top-level sections this version does not read (for example a
/// `[embeddings]` a user expected to apply everywhere) are named in a warning
/// rather than ignored silently.
pub async fn load_global_config(config_path: &Path) -> Result<GlobalConfig> {
    if !config_path.exists() {
        return Ok(GlobalConfig::default());
    }
    let content = tokio::fs::read_to_string(config_path).await?;
    let table: toml::Table = toml::from_str(&content)?;
    let unknown = GlobalConfig::unknown_top_level_keys(&table);
    if !unknown.is_empty() {
        warn_once(
            config_path,
            WarnKind::UnknownGlobalKeys,
            &format!(
                "global config file {} sets [{}], which is ignored: only [daemon] is \
                 read from the global config — model settings stay in each project's \
                 .engramdb/config.toml",
                config_path.display(),
                unknown.join("], [")
            ),
        );
    }
    let config: GlobalConfig = table.try_into()?;
    if let Err(e) = config.validate() {
        warn_once(
            config_path,
            WarnKind::Invalid,
            &format!(
                "global config file {} is invalid ({e}); continuing with the values as \
                 written — run `engramdb doctor` for details",
                config_path.display()
            ),
        );
    }
    Ok(config)
}

/// Load the global config from [`crate::paths::global_config_path`], falling
/// back to defaults on ANY failure — loudly, once per path per process.
///
/// This is what every `[daemon]` read goes through: the client resolve path,
/// the MCP heartbeat, `daemon run/status/stop/restart`, `doctor` and
/// `stats --daemon`. Daemon failures must never break an operation, so a bad
/// file means defaults, not an error.
pub async fn load_global_config_or_default() -> GlobalConfig {
    let config_path = match crate::paths::global_config_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot resolve the global config path ({e}); using defaults");
            return GlobalConfig::default();
        }
    };
    match load_global_config(&config_path).await {
        Ok(config) => config,
        Err(e) => {
            warn_once(
                &config_path,
                WarnKind::Unreadable,
                &format!(
                    "ignoring global config file {}: {e}; ALL settings in it are falling \
                     back to defaults — fix the file (see `engramdb doctor`) to restore them",
                    config_path.display()
                ),
            );
            GlobalConfig::default()
        }
    }
}

/// Which config problem a warning is about. Part of the dedup key, so one
/// path can carry more than one distinct warning (an invalid value *and* a
/// leftover `[daemon]` table are two problems, and the second must not be
/// swallowed because the first was already reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WarnKind {
    Invalid,
    Unreadable,
    LegacyDaemon,
    UnknownGlobalKeys,
}

/// Emit `tracing::warn!` for a config problem once per path and kind per
/// process, so per-tool-call config loads don't flood the log with the same
/// diagnosis.
fn warn_once(config_path: &Path, kind: WarnKind, message: &str) {
    static WARNED: OnceLock<Mutex<HashSet<(PathBuf, WarnKind)>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let fresh = warned
        .lock()
        .map(|mut set| set.insert((config_path.to_path_buf(), kind)))
        .unwrap_or(true);
    if fresh {
        tracing::warn!("{message}");
        #[cfg(test)]
        if let Ok(mut log) = emitted().lock() {
            log.push(message.to_string());
        }
    }
}

/// Every warning [`warn_once`] emitted in this test process, so a test can
/// assert what the user would see without installing a tracing subscriber.
#[cfg(test)]
fn emitted() -> &'static Mutex<Vec<String>> {
    static EMITTED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    EMITTED.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_load_config_returns_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let config = load_config(&config_path).await.unwrap();
        let default_config = EngramConfig::default();

        assert_eq!(
            config.retrieval.max_results,
            default_config.retrieval.max_results
        );
        assert_eq!(
            config.retrieval.relevance_threshold,
            default_config.retrieval.relevance_threshold
        );
    }

    #[tokio::test]
    async fn test_load_config_from_valid_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let toml_content = r#"
[retrieval]
max_results = 10
relevance_threshold = 0.8
include_expired = true

[retrieval.scoring.with_query]
semantic = 0.6
relevance = 0.25
scope = 0.1
trust = 0.05

[retrieval.scoring.scope_only]
relevance = 0.6
scope = 0.3
trust = 0.1

[retrieval.scoring.degraded]
relevance = 0.7
scope = 0.2
trust = 0.1

[trust_weights]
human = 0.95
agent = 0.85
inferred = 0.55
imported = 0.65
"#;
        tokio::fs::write(&config_path, toml_content).await.unwrap();

        let config = load_config(&config_path).await.unwrap();

        assert_eq!(config.retrieval.max_results, 10);
        assert_eq!(config.retrieval.relevance_threshold, 0.8);
        assert!(config.retrieval.include_expired);
        assert_eq!(config.trust_weights.human, 0.95);
        assert_eq!(config.trust_weights.agent, 0.85);
        assert_eq!(config.trust_weights.inferred, 0.55);
        assert_eq!(config.trust_weights.imported, 0.65);
    }

    #[tokio::test]
    async fn test_load_config_invalid_toml() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        tokio::fs::write(&config_path, "invalid { toml content")
            .await
            .unwrap();

        let result = load_config(&config_path).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::error::StorageError::Toml(_)
        ));
    }

    #[test]
    fn legacy_daemon_message_names_each_key_and_the_global_path() {
        let cfg: EngramConfig =
            toml::from_str("[daemon]\nidle_timeout_secs = 60\nsocket_path = \"/x\"\n").unwrap();
        let msg = legacy_daemon_message(Path::new("/p/.engramdb/config.toml"), &cfg).unwrap();
        assert!(msg.contains("daemon.idle_timeout_secs"), "{msg}");
        assert!(msg.contains("daemon.socket_path"), "{msg}");
        let global = crate::paths::global_config_path().unwrap();
        assert!(msg.contains(&global.display().to_string()), "{msg}");

        let cfg = EngramConfig::default();
        assert!(legacy_daemon_message(Path::new("/p"), &cfg).is_none());
    }

    #[tokio::test]
    async fn global_config_missing_means_defaults() {
        let dir = tempdir().unwrap();
        let cfg = load_global_config(&dir.path().join("config.toml"))
            .await
            .unwrap();
        assert!(cfg.daemon.enabled);
        assert_eq!(cfg.daemon.idle_timeout_secs, 900);
    }

    #[tokio::test]
    async fn global_config_reads_daemon_section_and_tolerates_unknown_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        tokio::fs::write(
            &path,
            "[daemon]\nidle_timeout_secs = 120\nuse_for_cli = false\n\n[embeddings]\nprovider = \"x\"\n",
        )
        .await
        .unwrap();
        let cfg = load_global_config(&path).await.unwrap();
        assert_eq!(cfg.daemon.idle_timeout_secs, 120);
        assert!(!cfg.daemon.use_for_cli);
    }

    #[tokio::test]
    async fn global_config_or_default_follows_the_config_dir() {
        // The test harness points ENGRAMDB_CONFIG_DIR at a per-process temp
        // dir, so this writes the real global path for this process only.
        let path = crate::paths::global_config_path().unwrap();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, "[daemon]\nidle_timeout_secs = 321\n")
            .await
            .unwrap();
        assert_eq!(
            load_global_config_or_default()
                .await
                .daemon
                .idle_timeout_secs,
            321
        );
        tokio::fs::write(&path, "not { toml").await.unwrap();
        assert_eq!(
            load_global_config_or_default()
                .await
                .daemon
                .idle_timeout_secs,
            900,
            "an unreadable global config falls back to defaults"
        );
    }

    /// Two projects that set different `[daemon]` values both still load,
    /// and each gets its own warning naming its keys and the global file —
    /// once per path, however often the config is reloaded.
    #[tokio::test]
    async fn each_project_with_daemon_keys_warns_once() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        let path_a = a.path().join("config.toml");
        let path_b = b.path().join("config.toml");
        tokio::fs::write(&path_a, "[daemon]\nidle_timeout_secs = 60\n")
            .await
            .unwrap();
        tokio::fs::write(
            &path_b,
            "[daemon]\nidle_timeout_secs = 3000\nsocket_path = \"/b.sock\"\n",
        )
        .await
        .unwrap();

        for _ in 0..3 {
            load_config_or_default(&path_a).await;
            load_config_or_default(&path_b).await;
        }

        let global = crate::paths::global_config_path().unwrap();
        let global = global.display().to_string();
        let log = emitted().lock().unwrap().clone();
        let about = |p: &Path| {
            log.iter()
                .filter(|m| m.contains(&p.display().to_string()))
                .cloned()
                .collect::<Vec<_>>()
        };
        let for_a = about(&path_a);
        let for_b = about(&path_b);
        assert_eq!(for_a.len(), 1, "one warning per path: {log:?}");
        assert_eq!(for_b.len(), 1, "one warning per path: {log:?}");
        assert!(for_a[0].contains("daemon.idle_timeout_secs") && for_a[0].contains(&global));
        assert!(
            for_b[0].contains("daemon.idle_timeout_secs")
                && for_b[0].contains("daemon.socket_path")
                && for_b[0].contains(&global),
            "{}",
            for_b[0]
        );
    }

    /// An invalid value and a leftover `[daemon]` table in one file are two
    /// problems; the second warning must not be swallowed by the first.
    #[tokio::test]
    async fn legacy_daemon_warning_is_not_masked_by_another_warning() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        tokio::fs::write(
            &path,
            "[nli]\nenabled = true\nmodel = \"m\"\ncontradiction_threshold = 7.0\n\
             max_comparisons = 1\nsimilarity_threshold = 0.5\n[daemon]\nenabled = false\n",
        )
        .await
        .unwrap();
        load_config_or_default(&path).await;
        let log = emitted().lock().unwrap().clone();
        let shown = path.display().to_string();
        assert!(
            log.iter()
                .any(|m| m.contains(&shown) && m.contains("daemon.enabled")),
            "{log:?}"
        );
    }
}
