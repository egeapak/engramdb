//! Decay strategies and configuration for time-based relevance adjustment.
//!
//! This module defines how a memory's relevance score decreases over time.
//! Strategies include:
//! - None: no decay (e.g., architectural decisions)
//! - Exponential: half-life based decay (e.g., debug notes)
//! - Linear: constant decay rate
//! - Step: discrete decay at intervals
//!
//! Each strategy can have a minimum floor value to prevent scores from
//! reaching zero. Different [`MemoryType`](super::MemoryType) values use
//! different default decay strategies.

use chrono::Duration;
use serde::{Deserialize, Serialize};

/// Strategy for how a memory's relevance score decays over time
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DecayStrategy {
    /// No decay - relevance stays constant
    None,
    /// Linear decay over time
    Linear,
    /// Exponential decay (half-life based)
    Exponential,
    /// Step function decay at specific intervals
    Step,
}

/// Decay configuration for a memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decay {
    /// The decay strategy to use
    pub strategy: DecayStrategy,

    /// Half-life for exponential decay (optional)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lazy_delta::deserialize"
    )]
    pub half_life: Option<Duration>,

    /// Time-to-live - absolute expiration (optional)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lazy_delta::deserialize"
    )]
    pub ttl: Option<Duration>,

    /// Minimum relevance floor (0.0 to 1.0, default 0.0)
    #[serde(default)]
    pub floor: f64,
}

/// Deserialize an optional `TimeDelta` in chrono's own `(secs, nanos)` form.
///
/// chrono's impl builds its "out of bounds" error with `ok_or`, so every
/// successful parse allocates an error message and drops it: two allocations
/// per field, paid for every decayed row a query reads from the index. This
/// accepts exactly what chrono accepts and builds the error only on failure.
mod lazy_delta {
    use chrono::Duration;
    use serde::{de::Error, Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        let Some((secs, nanos)) = Option::<(i64, i32)>::deserialize(d)? else {
            return Ok(None);
        };
        Duration::new(secs, nanos as u32)
            .map(Some)
            .ok_or_else(|| D::Error::custom("TimeDelta out of bounds"))
    }
}

impl Decay {
    /// Create a new Decay with the specified strategy
    pub fn new(strategy: DecayStrategy) -> Self {
        Self {
            strategy,
            half_life: None,
            ttl: None,
            floor: 0.0,
        }
    }

    /// Create a Decay with no decay
    pub fn none() -> Self {
        Self::new(DecayStrategy::None)
    }

    /// Create a Decay with no decay but a minimum floor
    pub fn none_with_floor(floor: f64) -> Self {
        Self {
            strategy: DecayStrategy::None,
            half_life: None,
            ttl: None,
            floor,
        }
    }

    /// Create an exponential decay with the given half-life
    pub fn exponential(half_life: Duration) -> Self {
        Self {
            strategy: DecayStrategy::Exponential,
            half_life: Some(half_life),
            ttl: None,
            floor: 0.0,
        }
    }

    /// Create a linear decay with the given TTL
    pub fn linear(ttl: Duration) -> Self {
        Self {
            strategy: DecayStrategy::Linear,
            half_life: None,
            ttl: Some(ttl),
            floor: 0.0,
        }
    }

    /// Set the floor value
    pub fn with_floor(mut self, floor: f64) -> Self {
        self.floor = floor;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_delta_accepts_and_refuses_what_chrono_does() {
        #[derive(Deserialize)]
        struct Chrono {
            half_life: Option<Duration>,
        }
        for json in [
            r#"{"strategy":"exponential","half_life":[86400,0]}"#,
            r#"{"strategy":"exponential","half_life":[-5,999999999]}"#,
            r#"{"strategy":"exponential","half_life":null}"#,
            r#"{"strategy":"exponential"}"#,
            r#"{"strategy":"exponential","half_life":[9223372036854775807,0]}"#,
            r#"{"strategy":"exponential","half_life":[1,2000000000]}"#,
            r#"{"strategy":"exponential","half_life":"1d"}"#,
        ] {
            let ours = serde_json::from_str::<Decay>(json).map(|d| d.half_life);
            let theirs = serde_json::from_str::<Chrono>(json).map(|c| c.half_life);
            match (ours, theirs) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{json}"),
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{json}"),
                (a, b) => panic!("{json}: ours {a:?}, chrono {b:?}"),
            }
        }
    }

    #[test]
    fn test_decay_none() {
        let decay = Decay::none();

        assert_eq!(decay.strategy, DecayStrategy::None);
        assert_eq!(decay.half_life, None);
        assert_eq!(decay.ttl, None);
        assert_eq!(decay.floor, 0.0);
    }

    #[test]
    fn test_decay_none_with_floor() {
        let decay = Decay::none_with_floor(0.5);

        assert_eq!(decay.strategy, DecayStrategy::None);
        assert_eq!(decay.floor, 0.5);
        assert_eq!(decay.half_life, None);
        assert_eq!(decay.ttl, None);
    }

    #[test]
    fn test_decay_exponential() {
        let decay = Decay::exponential(Duration::days(14));

        assert_eq!(decay.strategy, DecayStrategy::Exponential);
        assert_eq!(decay.half_life, Some(Duration::days(14)));
        assert_eq!(decay.ttl, None);
        assert_eq!(decay.floor, 0.0);
    }

    #[test]
    fn test_decay_linear() {
        let decay = Decay::linear(Duration::days(30));

        assert_eq!(decay.strategy, DecayStrategy::Linear);
        assert_eq!(decay.ttl, Some(Duration::days(30)));
        assert_eq!(decay.half_life, None);
        assert_eq!(decay.floor, 0.0);
    }
}
