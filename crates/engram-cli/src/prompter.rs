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

#[cfg(test)]
pub struct MockPrompter {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
}

#[cfg(test)]
impl MockPrompter {
    pub fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: std::sync::Mutex::new(
                responses.into_iter().map(|s| s.to_string()).collect(),
            ),
        }
    }

    fn pop(&self) -> Result<String> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("MockPrompter: no more responses in queue"))
    }
}

#[cfg(test)]
impl Prompter for MockPrompter {
    fn select(&self, _message: &str, _options: &[&str]) -> Result<String> {
        self.pop()
    }

    fn text(&self, _message: &str, default: Option<&str>) -> Result<String> {
        let val = self.pop()?;
        if val.is_empty() {
            Ok(default.unwrap_or("").to_string())
        } else {
            Ok(val)
        }
    }

    fn confirm(&self, _message: &str, default: bool) -> Result<bool> {
        let val = self.pop()?;
        match val.to_lowercase().as_str() {
            "true" | "yes" | "y" => Ok(true),
            "false" | "no" | "n" => Ok(false),
            "" => Ok(default),
            other => bail!("MockPrompter: cannot parse '{}' as bool", other),
        }
    }

    fn float_validated(&self, _message: &str, default: f64) -> Result<f64> {
        let val = self.pop()?;
        if val.is_empty() {
            return Ok(default);
        }
        let f: f64 = val.parse()?;
        if !(0.0..=1.0).contains(&f) {
            bail!("Value must be between 0.0 and 1.0, got {}", f);
        }
        Ok(f)
    }
}
