//! Which model repositories a project config may make this process load.
//!
//! Model selection is per project, and a project's `.engramdb/config.toml` is
//! committed: in a cloned repository its author wrote it. Embeddings, the
//! reranker and T5 resolve through fixed spec tables, so a config can only
//! pick among built-in models. `[nli].model` was different: it is a Hugging
//! Face repository id, and any id was downloaded and handed to ONNX Runtime
//! and `tokenizers`, in the shared daemon when one is up, at session start.
//!
//! The rule here: `[nli].model` must be one of the built-in NLI repositories,
//! unless the **global** config sets `[security] allow_custom_model_repos =
//! true`. A refused repository means NLI stays off for that project (its
//! default state). It never means a silent switch to another model.
//!
//! The opt-in is read once per process, from the process's own global config
//! file. The daemon reads its own copy, so a request's `ModelSelection`
//! cannot grant it. A changed opt-in therefore needs a daemon restart
//! (`engramdb daemon restart`).
//!
//! What a project config can still do, declared: turn on NLI, the reranker
//! and T5, and so make this process download and hold the built-in models.
//! Those are known repositories of bounded size.

use crate::types::config::NliConfig;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// The NLI repositories a project config may select without the opt-in.
pub fn builtin_nli_repos() -> Vec<&'static str> {
    #[cfg(feature = "onnxruntime")]
    {
        crate::nli::ALL_NLI_SPECS.iter().map(|s| s.repo).collect()
    }
    #[cfg(not(feature = "onnxruntime"))]
    {
        vec![crate::types::config::DEFAULT_NLI_MODEL_REPO]
    }
}

/// Whether `repo` may be loaded as the NLI model.
///
/// Built-in repositories always. Any other only with `allow_custom`, and
/// only when it has the shape of a repository id: the id is joined into a
/// path under the model cache, so `..` and separators beyond the one `/` are
/// never accepted.
pub fn nli_repo_permitted(repo: &str, allow_custom: bool) -> Result<(), String> {
    if builtin_nli_repos().contains(&repo) {
        return Ok(());
    }
    if !crate::types::config::is_valid_hf_repo_id(repo) {
        return Err(format!(
            "[nli].model {repo:?} is not a Hugging Face repository id (owner/name)"
        ));
    }
    if allow_custom {
        return Ok(());
    }
    Err(format!(
        "[nli].model {repo:?} is not a built-in NLI model ({}). A project config cannot \
         select another repository unless the global config file sets [security] \
         allow_custom_model_repos = true; NLI stays off for this project",
        builtin_nli_repos().join(", ")
    ))
}

/// `[security] allow_custom_model_repos` from this process's global config
/// file, read once. Any failure to read it means `false`.
pub fn custom_repos_allowed() -> bool {
    #[cfg(test)]
    if let Some(forced) = *test_override().lock().unwrap_or_else(|e| e.into_inner()) {
        return forced;
    }
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(read_opt_in)
}

/// A synchronous read: the callers (`model_demands`, the model loaders) are
/// not async, and the file is a few lines.
fn read_opt_in() -> bool {
    let Ok(path) = crate::storage::paths::global_config_path() else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    toml::from_str::<crate::types::GlobalConfig>(&content)
        .map(|c| c.security.allow_custom_model_repos)
        .unwrap_or(false)
}

#[cfg(test)]
fn test_override() -> &'static Mutex<Option<bool>> {
    static OVERRIDE: Mutex<Option<bool>> = Mutex::new(None);
    &OVERRIDE
}

/// Force the opt-in for this test process (nextest runs one per test).
#[cfg(test)]
pub(crate) fn set_custom_repos_allowed_for_test(allowed: bool) {
    *test_override().lock().unwrap_or_else(|e| e.into_inner()) = Some(allowed);
}

/// The NLI model `config` selects, when NLI is enabled **and** the repository
/// is permitted. `None` otherwise.
///
/// Every place that would load, lease or wire an NLI model asks this, so a
/// refused repository is treated exactly like `[nli].enabled = false`. The
/// refusal is logged once per repository.
pub fn permitted_nli_model(config: &NliConfig) -> Option<&str> {
    if !config.enabled {
        return None;
    }
    match nli_repo_permitted(&config.model, custom_repos_allowed()) {
        Ok(()) => Some(config.model.as_str()),
        Err(reason) => {
            static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
            let fresh = WARNED
                .get_or_init(|| Mutex::new(HashSet::new()))
                .lock()
                .map(|mut set| set.insert(config.model.clone()))
                .unwrap_or(true);
            if fresh {
                tracing::warn!("{reason}");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_repos_are_always_permitted() {
        for repo in builtin_nli_repos() {
            assert!(nli_repo_permitted(repo, false).is_ok(), "{repo}");
        }
        assert!(builtin_nli_repos().contains(&crate::types::config::DEFAULT_NLI_MODEL_REPO));
    }

    #[test]
    fn a_custom_repo_needs_the_opt_in() {
        let err = nli_repo_permitted("attacker/repo", false).unwrap_err();
        assert!(err.contains("allow_custom_model_repos"), "{err}");
        assert!(nli_repo_permitted("attacker/repo", true).is_ok());
    }

    #[test]
    fn a_malformed_id_is_refused_even_with_the_opt_in() {
        for bad in ["../../etc", "a/b/c", "noslash", "a/..", "", "a b/c", "/abs"] {
            assert!(nli_repo_permitted(bad, true).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_refused_repo_reads_as_nli_off() {
        let mut nli = NliConfig {
            enabled: true,
            ..NliConfig::default()
        };
        assert_eq!(permitted_nli_model(&nli), Some(nli.model.as_str()));
        nli.model = "attacker/repo".to_string();
        assert_eq!(permitted_nli_model(&nli), None);
        set_custom_repos_allowed_for_test(true);
        assert_eq!(permitted_nli_model(&nli), Some("attacker/repo"));
        nli.enabled = false;
        assert_eq!(permitted_nli_model(&nli), None);
    }

    /// The opt-in comes from the global config file of this process.
    #[test]
    fn the_opt_in_is_read_from_the_global_config() {
        assert!(!read_opt_in(), "no global config: off");
        let path = crate::storage::paths::global_config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[security]\nallow_custom_model_repos = true\n").unwrap();
        assert!(read_opt_in());
        std::fs::write(&path, "not { toml").unwrap();
        assert!(!read_opt_in(), "an unreadable file grants nothing");
    }
}
