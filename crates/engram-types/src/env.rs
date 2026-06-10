//! Runtime environment overrides shared across front-ends.

/// Whether the shared embedding daemon should be bypassed in favour of
/// in-process model loading, as requested by the `ENGRAMDB_IN_PROCESS`
/// environment variable.
///
/// Truthy values (case-insensitive): `1`, `true`, `yes`, `on`. Anything else —
/// including an unset variable — is `false`.
///
/// Both the CLI (`--in-process` flag ladder) and the MCP server consult this so
/// the env var behaves identically regardless of front-end. The MCP server has
/// no equivalent flag, so this env var is its only knob for forcing in-process.
pub fn in_process_override() -> bool {
    std::env::var("ENGRAMDB_IN_PROCESS")
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::in_process_override;

    /// Serialize env mutation across these tests (process-global state).
    fn with_var(value: Option<&str>, f: impl FnOnce()) {
        let prev = std::env::var("ENGRAMDB_IN_PROCESS").ok();
        match value {
            Some(v) => std::env::set_var("ENGRAMDB_IN_PROCESS", v),
            None => std::env::remove_var("ENGRAMDB_IN_PROCESS"),
        }
        f();
        match prev {
            Some(v) => std::env::set_var("ENGRAMDB_IN_PROCESS", v),
            None => std::env::remove_var("ENGRAMDB_IN_PROCESS"),
        }
    }

    #[test]
    fn truthy_values_enable_override() {
        for v in ["1", "true", "TRUE", "Yes", "on"] {
            with_var(Some(v), || {
                assert!(in_process_override(), "{v:?} should be truthy");
            });
        }
    }

    #[test]
    fn falsy_or_unset_disables_override() {
        for v in ["0", "false", "no", "off", ""] {
            with_var(Some(v), || {
                assert!(!in_process_override(), "{v:?} should be falsy");
            });
        }
        with_var(None, || assert!(!in_process_override()));
    }
}
