//! Directory-grouping strategy for the CLI `projects list` output.
//!
//! This enum is a *configuration value* (it is stored in `[cli].project_list_grouping`),
//! so it lives in the `types` foundation rather than in the `engram-cli` crate
//! that renders with it. Keeping it here lets `types::config` reference the
//! default without the config crate depending "upward" on the CLI. Only the
//! human-readable CLI output honors it — the MCP surface and every `--json`
//! path stay a flat array regardless.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How the CLI `projects list` groups projects under filesystem-directory
/// headers. The worktree tree (sub-projects nested under their real parent) is
/// rendered in *every* mode; this only controls the directory headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectListGrouping {
    /// Print a directory header above every project, even folders that hold a
    /// single project. Each project line shows just its basename.
    Always,
    /// Print a directory header only for folders that contain two or more
    /// projects; a folder with a single project renders inline on one
    /// full-path line. The readable middle ground and the default.
    #[default]
    Auto,
    /// No directory headers at all — a flat list of full-path lines (still
    /// sorted by path, still nesting worktrees under their parent).
    None,
}

impl ProjectListGrouping {
    /// Parse a grouping mode from a string (case-insensitive, with aliases).
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "always" | "all" | "full" => Ok(Self::Always),
            "auto" | "smart" => Ok(Self::Auto),
            "none" | "off" | "flat" => Ok(Self::None),
            _ => anyhow::bail!(
                "Invalid project list grouping '{}'. Valid options: always, auto, none",
                s
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_always_aliases() {
        for s in ["always", "all", "full", "ALWAYS", "Full"] {
            assert_eq!(
                ProjectListGrouping::parse(s).unwrap(),
                ProjectListGrouping::Always
            );
        }
    }

    #[test]
    fn parse_auto_aliases() {
        for s in ["auto", "smart", "AUTO", "Smart"] {
            assert_eq!(
                ProjectListGrouping::parse(s).unwrap(),
                ProjectListGrouping::Auto
            );
        }
    }

    #[test]
    fn parse_none_aliases() {
        for s in ["none", "off", "flat", "NONE", "Off"] {
            assert_eq!(
                ProjectListGrouping::parse(s).unwrap(),
                ProjectListGrouping::None
            );
        }
    }

    #[test]
    fn parse_invalid_is_error() {
        let err = ProjectListGrouping::parse("nonsense")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid project list grouping"));
        assert!(err.contains("nonsense"));
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(ProjectListGrouping::default(), ProjectListGrouping::Auto);
    }

    #[test]
    fn serde_uses_lowercase_names() {
        // `#[serde(rename_all = "lowercase")]` is what the on-disk
        // `[cli].project_list_grouping` field relies on; keep it stable.
        assert_eq!(
            serde_json::to_string(&ProjectListGrouping::Always).unwrap(),
            "\"always\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectListGrouping::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectListGrouping::None).unwrap(),
            "\"none\""
        );
    }

    #[test]
    fn serde_round_trip() {
        for mode in [
            ProjectListGrouping::Always,
            ProjectListGrouping::Auto,
            ProjectListGrouping::None,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: ProjectListGrouping = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }
}
