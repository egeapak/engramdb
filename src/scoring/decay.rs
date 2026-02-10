use chrono::{DateTime, Utc};

use crate::types::{Decay, DecayStrategy, Memory};

/// Calculate the decay factor for a memory based on its age and decay configuration.
///
/// # Arguments
/// * `created_at` - When the memory was created
/// * `now` - Current timestamp
/// * `decay` - Optional decay configuration
///
/// # Returns
/// Decay factor from 0.0 to 1.0
///
/// # Strategy Details
/// - None: Always returns 1.0
/// - Linear: factor = 1.0 - (age / ttl), clamped to [floor, 1.0]
/// - Exponential: factor = 0.5^(age / half_life), clamped to [floor, 1.0]
/// - Step: factor = if age < ttl { 1.0 } else { floor }
pub fn decay_factor(created_at: DateTime<Utc>, now: DateTime<Utc>, decay: &Option<Decay>) -> f64 {
    let Some(decay_config) = decay else {
        return 1.0;
    };

    let age = now.signed_duration_since(created_at);
    let age_secs = age.num_seconds() as f64;

    match decay_config.strategy {
        DecayStrategy::None => 1.0,

        DecayStrategy::Linear => {
            if let Some(ttl) = decay_config.ttl {
                let ttl_secs = ttl.num_seconds() as f64;
                if age_secs >= ttl_secs {
                    decay_config.floor
                } else {
                    let factor = 1.0 - (age_secs / ttl_secs);
                    factor.max(decay_config.floor).min(1.0)
                }
            } else {
                // No TTL specified, no decay
                1.0
            }
        }

        DecayStrategy::Exponential => {
            if let Some(half_life) = decay_config.half_life {
                let half_life_secs = half_life.num_seconds() as f64;
                let exponent = age_secs / half_life_secs;
                let factor = 0.5_f64.powf(exponent);
                factor.max(decay_config.floor).min(1.0)
            } else {
                // No half-life specified, no decay
                1.0
            }
        }

        DecayStrategy::Step => {
            if let Some(ttl) = decay_config.ttl {
                let ttl_secs = ttl.num_seconds() as f64;
                if age_secs < ttl_secs {
                    1.0
                } else {
                    decay_config.floor
                }
            } else {
                // No TTL specified, no decay
                1.0
            }
        }
    }
}

/// Calculate the effective relevance of a memory, accounting for both criticality and decay.
///
/// # Arguments
/// * `memory` - The memory to calculate relevance for
/// * `now` - Current timestamp
///
/// # Returns
/// Effective relevance score = criticality * decay_factor
pub fn effective_relevance(memory: &Memory, now: DateTime<Utc>) -> f64 {
    memory.criticality * decay_factor(memory.created_at, now, &memory.decay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_decay_factor_none() {
        let created_at = Utc::now() - Duration::days(10);
        let now = Utc::now();
        let decay = Some(Decay::none());

        let factor = decay_factor(created_at, now, &decay);
        assert_eq!(factor, 1.0);
    }

    #[test]
    fn test_decay_factor_no_config() {
        let created_at = Utc::now() - Duration::days(10);
        let now = Utc::now();

        let factor = decay_factor(created_at, now, &None);
        assert_eq!(factor, 1.0);
    }

    #[test]
    fn test_decay_factor_linear() {
        let now = Utc::now();
        let created_at = now - Duration::days(5);
        let decay = Some(Decay::linear(Duration::days(10)));

        let factor = decay_factor(created_at, now, &decay);
        // 5 days / 10 days = 0.5, so factor = 1.0 - 0.5 = 0.5
        assert!((factor - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_decay_factor_linear_expired() {
        let now = Utc::now();
        let created_at = now - Duration::days(15);
        let decay = Some(Decay::linear(Duration::days(10)).with_floor(0.1));

        let factor = decay_factor(created_at, now, &decay);
        // Age >= TTL, should return floor
        assert_eq!(factor, 0.1);
    }

    #[test]
    fn test_decay_factor_exponential() {
        let now = Utc::now();
        let created_at = now - Duration::days(7);
        let decay = Some(Decay::exponential(Duration::days(7)));

        let factor = decay_factor(created_at, now, &decay);
        // 7 days / 7 days = 1 half-life, so factor = 0.5^1 = 0.5
        assert!((factor - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_decay_factor_exponential_with_floor() {
        let now = Utc::now();
        let created_at = now - Duration::days(70);
        let decay = Some(Decay::exponential(Duration::days(7)).with_floor(0.1));

        let factor = decay_factor(created_at, now, &decay);
        // 70 days / 7 days = 10 half-lives, 0.5^10 = 0.000976... clamped to floor
        assert_eq!(factor, 0.1);
    }

    #[test]
    fn test_decay_factor_step() {
        let now = Utc::now();
        let created_at = now - Duration::days(5);
        let decay = Some(Decay {
            strategy: DecayStrategy::Step,
            half_life: None,
            ttl: Some(Duration::days(10)),
            floor: 0.2,
        });

        let factor = decay_factor(created_at, now, &decay);
        // Age < TTL, should return 1.0
        assert_eq!(factor, 1.0);
    }

    #[test]
    fn test_decay_factor_step_expired() {
        let now = Utc::now();
        let created_at = now - Duration::days(15);
        let decay = Some(Decay {
            strategy: DecayStrategy::Step,
            half_life: None,
            ttl: Some(Duration::days(10)),
            floor: 0.2,
        });

        let factor = decay_factor(created_at, now, &decay);
        // Age >= TTL, should return floor
        assert_eq!(factor, 0.2);
    }
}
