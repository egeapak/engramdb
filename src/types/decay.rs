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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub half_life: Option<Duration>,

    /// Time-to-live - absolute expiration (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<Duration>,

    /// Minimum relevance floor (0.0 to 1.0, default 0.0)
    #[serde(default)]
    pub floor: f64,
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
