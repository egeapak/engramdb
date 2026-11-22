//! The raw-output hook surface: stdin in, the protocol document and the
//! diagnostics out. The only file in `engram-cli` exempt from the
//! `formatter-output` CI check.

use super::*;

/// The warning for a hook command line clap could not parse (an unknown
/// flag or a bad flag value: version skew between the hook wiring and the
/// binary). Printed from here because this file is the declared raw-output
/// surface; the hook is not run and records no stats.
pub fn warn_unparseable_hook_args(error_line: &str) {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "engramdb hook: ignoring unparseable arguments (version skew?): {error_line}"
    );
}

/// Report a `hook` invocation this binary cannot serve, then let the caller
/// exit 0.
///
/// Lives here rather than beside its call site in `lib.rs` so that the whole
/// raw-output hook surface sits in one file — the CI check that forbids bare
/// print macros in `engram-cli` exempts exactly this module and `output.rs`.
///
/// A hook that exits non-zero is a *blocking* error in Claude Code: a broken
/// `UserPromptSubmit` rejects every prompt, a broken `PreToolUse` every
/// Read/Write/Edit. Because the hook wiring is installed independently of the
/// binary, a plugin update alone can name an event this build never shipped —
/// so the only safe response is to degrade to "no context" the way every other
/// hook failure already does.
///
/// The message goes to **stderr on purpose**: on `UserPromptSubmit` (and
/// `SessionStart`) Claude Code injects a hook's stdout into the model's
/// context on exit 0, so writing there would quietly poison every prompt with
/// a diagnostic.
pub fn warn_unknown_hook(name: Option<&str>) {
    use std::io::Write as _;
    let supported = crate::supported_hook_subcommands().join(", ");
    // `writeln!` on a locked handle, not `eprintln!`: a closed stderr must not
    // panic a hook (the release profile aborts on panic).
    let mut err = std::io::stderr().lock();
    let _ = match name {
        Some(name) => writeln!(
            err,
            "engramdb {} does not support `hook {name}`. Your Claude Code hook \
             configuration is newer than this binary — reinstall with \
             `cargo install --git https://github.com/egeapak/engramdb --force`. \
             Supported here: {supported}. (Continuing without context; this hook is a no-op.)",
            env!("CARGO_PKG_VERSION"),
        ),
        None => writeln!(
            err,
            "engramdb hook: no hook event given. Expected one of: {supported}."
        ),
    };
}

/// The most stdin a hook reads: half the daemon's 64 MiB frame cap, because
/// the `Hook` frame carries stdin JSON-escaped and escaping can double it.
pub const MAX_HOOK_STDIN: u64 = 32 * 1024 * 1024;

/// Read the hook event from stdin. Hooks must never fail on their input, so
/// a closed stdin reads as empty and invalid UTF-8 is replaced.
///
/// Bounded by [`MAX_HOOK_STDIN`]. Declared loss path: an event larger than
/// the cap is cut, fails to parse, and the hook returns no context (a warning
/// says so). No read timeout: Claude Code owns the hook timeout.
pub fn read_stdin() -> String {
    let (text, capped) = read_capped(std::io::stdin().lock(), MAX_HOOK_STDIN);
    if capped {
        tracing::warn!(
            "engramdb hook: event on stdin exceeds {MAX_HOOK_STDIN} bytes; ignoring it (no context)"
        );
    }
    text
}

/// Read at most `cap` bytes from `r`; `true` when more was available.
pub(crate) fn read_capped(r: impl Read, cap: u64) -> (String, bool) {
    let mut bytes = Vec::new();
    // One byte over the cap tells "exactly cap" from "more than cap".
    let _ = r.take(cap.saturating_add(1)).read_to_end(&mut bytes);
    let capped = bytes.len() as u64 > cap;
    if capped {
        bytes.truncate(cap as usize);
    }
    (String::from_utf8_lossy(&bytes).into_owned(), capped)
}

/// Print a hook's output: the hook protocol document Claude Code parses off
/// stdout.
///
/// Never `println!`: it panics when stdout is closed (Claude Code gave up on
/// the hook), and the release profile aborts on panic. A failed write is
/// ignored; nobody is reading.
pub fn print_hook_output(output: Option<&str>) {
    use std::io::Write as _;
    if let Some(json) = output {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
}
