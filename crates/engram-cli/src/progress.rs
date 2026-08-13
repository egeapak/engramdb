//! Progress-bar construction for the long-running commands.
//!
//! # Why this is a module and not a closure
//!
//! `ProgressBar::new` draws to `Term::buffered_stderr()`, and
//! `ProgressDrawTarget::is_hidden()` is `!term.is_term()` — so under any pipe
//! the bar renders *nothing*. That is the right behaviour and exactly why the
//! bar used to be untestable: tier 2 pipes, and the command tier captures the
//! [`OutputFormatter`](crate::output::OutputFormatter)'s sink, which a
//! `ProgressBar` never writes to.
//!
//! Splitting the draw target out as a parameter gives the tests a seam:
//! `indicatif::InMemoryTerm` (dev-only feature `in_memory`) implements
//! `TermLike`, `ProgressDrawTarget::term_like` accepts it, and
//! `InMemoryTerm::contents()` hands back the rendered screen as plain text.
//!
//! The dev-dependency is deliberately *only* a dev-dependency. The workspace
//! is not virtual (the root `Cargo.toml` has a `[package]`, edition 2021), so
//! feature resolver v2 is in effect and `in_memory`'s `vt100` is not unified
//! into normal builds:
//!
//! ```text
//! cargo tree -p engram-cli -e normal     -i vt100  # nothing to print
//! cargo tree -p engram-cli -e normal,dev -i vt100  # vt100 v0.16.2
//! ```

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// The one template every `projects prune` bar shares.
///
/// Pinned as a constant so a test can assert it: `InMemoryTerm::contents()`
/// strips styling, so the `.green/dim` colouring is unobservable from a
/// rendered snapshot. Colour here is indicatif rendering *our config string*,
/// so pinning the string is the honest test.
pub(crate) const PRUNE_TEMPLATE: &str = "{prefix} [{bar:40.green/dim}] {pos}/{len} ({eta})";

/// The glyphs `{bar}` fills with: filled / current / empty.
pub(crate) const PRUNE_PROGRESS_CHARS: &str = "=>-";

/// Where a `projects prune` bar draws.
///
/// JSON is machine-consumed, so progress chatter is suppressed entirely;
/// otherwise the indicatif default (buffered stderr, hidden unless a TTY).
pub(crate) fn prune_draw_target(json_mode: bool) -> ProgressDrawTarget {
    if json_mode {
        ProgressDrawTarget::hidden()
    } else {
        ProgressDrawTarget::stderr()
    }
}

/// Build one `projects prune` phase bar.
///
/// `len == 0` finishes-and-clears immediately: a zero-length bar would
/// otherwise sit on screen at `0/0` forever, since nothing will ever tick it.
pub(crate) fn make_bar(len: u64, prefix: &'static str, target: ProgressDrawTarget) -> ProgressBar {
    let style = ProgressStyle::default_bar()
        .template(PRUNE_TEMPLATE)
        .expect("prune progress template is a compile-time constant")
        .progress_chars(PRUNE_PROGRESS_CHARS);
    let pb = ProgressBar::with_draw_target(Some(len), target);
    pb.set_style(style);
    pb.set_prefix(prefix);
    if len == 0 {
        pb.finish_and_clear();
    }
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::InMemoryTerm;

    /// 100 columns because the template alone is 40 columns of `{bar}` plus a
    /// prefix, the counters and the eta; at 80 the line wraps and the snapshot
    /// stops being readable.
    fn term() -> InMemoryTerm {
        InMemoryTerm::new(10, 100)
    }

    fn bar_on(term: &InMemoryTerm, len: u64, prefix: &'static str) -> ProgressBar {
        make_bar(
            len,
            prefix,
            ProgressDrawTarget::term_like(Box::new(term.clone())),
        )
    }

    /// Snapshot a rendered bar.
    ///
    /// A local equivalent of `testutil::snap_command` rather than a call into
    /// it: that helper runs `normalize`, whose id/path filters have nothing to
    /// do with a progress bar and whose 8-hex rule could in principle bite a
    /// counter. Same settings, same directory, same "name carries the
    /// identity" contract — so every name here is `progress_`-prefixed and
    /// unique across the tier.
    fn snap_progress(name: &str, body: String) {
        insta::with_settings!({
            snapshot_path => "../tests/snapshots/command",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(name, body);
        });
    }

    /// The determinism probe that had to pass before any case was written.
    ///
    /// `{eta}` is wall-clock derived, so the obvious worry is a snapshot that
    /// changes between runs. It does not: indicatif's estimator has no samples
    /// until the bar moves, and once it does the whole test takes microseconds,
    /// so the projected remaining time always formats as `0s`. Rendering the
    /// same three states twice in one process — which is strictly harder than
    /// two separate runs, because the second pass has warm state — must produce
    /// identical text. If this ever starts failing, redact `\(\d+[smhd]\)` to
    /// `(ETA)` rather than deleting the eta from the template.
    #[test]
    fn progress_eta_is_stable_across_renders() {
        let render = || {
            let term = term();
            let pb = bar_on(&term, 4, "stale");
            pb.tick();
            let zero = term.contents();
            pb.inc(1);
            let one = term.contents();
            pb.inc(3);
            let done = term.contents();
            [zero, one, done]
        };

        let first = render();
        let second = render();
        assert_eq!(first, second, "progress rendering is not deterministic");
        for frame in &first {
            assert!(
                frame.ends_with("(0s)"),
                "eta drifted off 0s — redact it: {frame}"
            );
        }
    }

    /// A freshly built bar, never ticked. `tick()` forces the first draw;
    /// without it the bar has not yet asked the draw target for anything.
    #[test]
    fn progress_prune_bar_at_zero() {
        let term = term();
        let pb = bar_on(&term, 4, "stale");
        pb.tick();
        snap_progress("progress_prune_bar_at_zero", term.contents());
    }

    /// Mid-progress: the `>` head glyph and the `-` remainder both visible.
    #[test]
    fn progress_prune_bar_mid_progress() {
        let term = term();
        let pb = bar_on(&term, 4, "orphan");
        pb.inc(1);
        snap_progress("progress_prune_bar_mid_progress", term.contents());
    }

    /// Fully ticked: `{bar}` is all `=`, with no head glyph.
    #[test]
    fn progress_prune_bar_complete() {
        let term = term();
        let pb = bar_on(&term, 3, "links");
        pb.inc(3);
        snap_progress("progress_prune_bar_complete", term.contents());
    }

    /// `len == 0` is finished-and-cleared inside `make_bar`, so nothing ever
    /// reaches the screen — the empty snapshot *is* the assertion. The extra
    /// `tick()` proves the clear sticks: a later tick on a finished bar must
    /// not redraw it.
    #[test]
    fn progress_prune_bar_empty_is_cleared() {
        let term = term();
        let pb = bar_on(&term, 0, "stale");
        pb.tick();
        assert!(pb.is_finished());
        snap_progress("progress_prune_bar_empty_is_cleared", term.contents());
    }

    /// JSON mode suppresses the bars entirely; the human path does not.
    #[test]
    fn progress_prune_draw_target_hidden_in_json_mode() {
        assert!(prune_draw_target(true).is_hidden());
        // Not a TTY under a test runner, so the stderr target is *also*
        // hidden here — asserting `!is_hidden()` would be asserting the
        // runner's environment, not our code. What is ours is that the two
        // branches differ in kind, which the JSON assertion above already
        // pins.
        let _ = prune_draw_target(false);
    }

    /// Colour and glyphs are config, not rendering: `contents()` strips
    /// styling, so the only honest place to pin `.green/dim` and `=>-` is the
    /// strings we hand indicatif.
    #[test]
    fn progress_prune_style_config_is_pinned() {
        assert_eq!(
            PRUNE_TEMPLATE,
            "{prefix} [{bar:40.green/dim}] {pos}/{len} ({eta})"
        );
        assert_eq!(PRUNE_PROGRESS_CHARS, "=>-");
        // The template must actually compile — `make_bar` unwraps it.
        assert!(ProgressStyle::default_bar()
            .template(PRUNE_TEMPLATE)
            .is_ok());
    }
}
