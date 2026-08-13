//! Abstraction over interactive prompts for testability.
//!
//! Production code uses [`InquirePrompter`] which delegates to the `inquire` crate.
//! Tests use [`MockPrompter`] which replays scripted responses from a queue.

use anyhow::Result;

/// Abstraction over interactive prompts for testability.
pub trait Prompter: Send + Sync {
    /// Present a list of options and return the selected one.
    fn select(&self, message: &str, options: &[&str]) -> Result<String>;

    /// Prompt for free-form text input. Returns the default (or empty string) if accepted.
    fn text(&self, message: &str, default: Option<&str>) -> Result<String>;

    /// Prompt for a yes/no confirmation.
    fn confirm(&self, message: &str, default: bool) -> Result<bool>;

    /// Prompt for an f64 with validation (value must be in 0.0..=1.0).
    fn float_validated(&self, message: &str, default: f64) -> Result<f64>;
}

/// Production prompter backed by the `inquire` crate.
pub struct InquirePrompter;

impl Prompter for InquirePrompter {
    fn select(&self, message: &str, options: &[&str]) -> Result<String> {
        let selected = inquire::Select::new(message, options.to_vec()).prompt()?;
        Ok(selected.to_string())
    }

    fn text(&self, message: &str, default: Option<&str>) -> Result<String> {
        let mut prompt = inquire::Text::new(message);
        if let Some(d) = default {
            prompt = prompt.with_default(d);
        }
        Ok(prompt.prompt()?)
    }

    fn confirm(&self, message: &str, default: bool) -> Result<bool> {
        Ok(inquire::Confirm::new(message)
            .with_default(default)
            .prompt()?)
    }

    fn float_validated(&self, message: &str, default: f64) -> Result<f64> {
        let val = inquire::CustomType::<f64>::new(message)
            .with_default(default)
            .with_error_message("Please enter a number between 0.0 and 1.0")
            .with_validator(|val: &f64| {
                if *val >= 0.0 && *val <= 1.0 {
                    Ok(inquire::validator::Validation::Valid)
                } else {
                    Ok(inquire::validator::Validation::Invalid(
                        "Value must be between 0.0 and 1.0".into(),
                    ))
                }
            })
            .prompt()?;
        Ok(val)
    }
}

/// Mock prompter for tests. Replays scripted responses from a queue.
#[cfg(test)]
use anyhow::bail;

/// Mock prompter for tests: replays scripted responses, and records the
/// dialogue.
///
/// The recording is the point of [`MockPrompter::transcript`]. A prompt's
/// wording, its option list and its default are user-visible interface, but
/// they reach the terminal through `inquire`, not through `OutputFormatter` —
/// so no capture of the formatter can see them, and before this they were
/// asserted nowhere. Rendering what was asked next to what was answered turns
/// an interactive flow into something a snapshot can hold.
#[cfg(test)]
pub struct MockPrompter {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    asked: std::sync::Mutex<Vec<String>>,
    /// Deliberately not `asked.len()`. `record` runs only after a response is
    /// successfully popped, so the transcript holds answered prompts; this
    /// counts *issued* ones, including the exhausted-queue case a caller may
    /// have absorbed. See [`MockPrompter::prompt_count`].
    prompts: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl MockPrompter {
    pub fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: std::sync::Mutex::new(
                responses.into_iter().map(|s| s.to_string()).collect(),
            ),
            asked: std::sync::Mutex::new(Vec::new()),
            prompts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many prompts have been issued.
    ///
    /// An exhausted queue returns `Err`, not a panic, and a caller that maps
    /// the error to a default (or declines on it) absorbs the mistake — so
    /// "this path must not prompt" cannot be tested by supplying an empty
    /// queue alone. Assert on this counter instead.
    #[allow(dead_code)]
    pub fn prompt_count(&self) -> usize {
        self.prompts.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn pop(&self) -> Result<String> {
        self.prompts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("MockPrompter: no more responses in queue"))
    }

    /// Record one `question → answer` exchange.
    ///
    /// `answer` is the *resolved* value, not the raw queue entry, so an empty
    /// scripted response shows the default it fell back to rather than a blank.
    ///
    /// An answer that resolves to nothing renders as `(empty)` rather than
    /// leaving the line to end in a space: a trailing space in a `.snap` is
    /// invisible in review and gets stripped by editors and pre-commit hooks,
    /// which would fail the snapshot for a reason nobody can see. It also says
    /// what actually happened — the prompt was accepted with no value.
    fn record(&self, question: String, answer: &str) {
        let shown = if answer.is_empty() { "(empty)" } else { answer };
        self.asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("? {question} → {shown}"));
    }

    /// The dialogue so far, one line per prompt, newline-terminated.
    ///
    /// Empty string when nothing was asked — so a flow that skipped its
    /// prompts (`--yes`, an early return) is visibly distinct from one that
    /// asked and was declined.
    pub fn transcript(&self) -> String {
        let asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        asked.iter().map(|line| format!("{line}\n")).collect()
    }
}

#[cfg(test)]
impl Prompter for MockPrompter {
    fn select(&self, message: &str, options: &[&str]) -> Result<String> {
        let answer = self.pop()?;
        self.record(format!("{message} [{}]", options.join(", ")), &answer);
        Ok(answer)
    }

    fn text(&self, message: &str, default: Option<&str>) -> Result<String> {
        let val = self.pop()?;
        let resolved = if val.is_empty() {
            default.unwrap_or("").to_string()
        } else {
            val
        };
        let question = match default {
            Some(d) if !d.is_empty() => format!("{message} [default: {d}]"),
            _ => message.to_string(),
        };
        self.record(question, &resolved);
        Ok(resolved)
    }

    fn confirm(&self, message: &str, default: bool) -> Result<bool> {
        let val = self.pop()?;
        let resolved = match val.to_lowercase().as_str() {
            "true" | "yes" | "y" => true,
            "false" | "no" | "n" => false,
            "" => default,
            other => bail!("MockPrompter: cannot parse '{}' as bool", other),
        };
        // `[y/N]` / `[Y/n]` mirrors how inquire renders the default, so the
        // transcript shows which way a bare Enter would have gone.
        let hint = if default { "Y/n" } else { "y/N" };
        self.record(
            format!("{message} [{hint}]"),
            if resolved { "yes" } else { "no" },
        );
        Ok(resolved)
    }

    fn float_validated(&self, message: &str, default: f64) -> Result<f64> {
        let val = self.pop()?;
        let resolved = if val.is_empty() {
            default
        } else {
            let f: f64 = val.parse()?;
            if !(0.0..=1.0).contains(&f) {
                // Record before failing: the transcript of a rejected answer is
                // exactly what the error case needs to show.
                self.record(format!("{message} [default: {default}]"), &val);
                bail!("Value must be between 0.0 and 1.0, got {}", f);
            }
            f
        };
        self.record(
            format!("{message} [default: {default}]"),
            &resolved.to_string(),
        );
        Ok(resolved)
    }
}
