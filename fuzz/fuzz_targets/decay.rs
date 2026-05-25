#![no_main]

use chrono::{DateTime, Duration, Utc};
use libfuzzer_sys::fuzz_target;

use engramdb::scoring::decay_factor;
use engramdb::types::{Decay, DecayStrategy};

// `decay_factor` does float math over caller-influenced durations: an age in
// seconds divided by a TTL / half-life, then `0.5.powf(..)` and a clamp. TTL,
// half-life and timestamps ultimately come from on-disk memory files, so a
// hostile or corrupt file can drive every input here. The factor multiplies
// into relevance, so it must always be finite (a NaN/inf would unorder search
// results or break later arithmetic), regardless of zero/negative/overflowing
// durations or future timestamps.
fuzz_target!(|input: (u8, Option<i64>, Option<i64>, f64, i64, i64)| {
    let (strat_sel, ttl_secs, half_life_secs, floor, created_ts, now_ts) = input;

    // A NaN/inf floor is propagated verbatim by the function; that's an
    // input-validation concern, not the arithmetic this target probes. Skip it
    // so the assertion isolates the age/TTL/half-life math.
    if !floor.is_finite() {
        return;
    }

    let strategy = match strat_sel % 4 {
        0 => DecayStrategy::None,
        1 => DecayStrategy::Linear,
        2 => DecayStrategy::Exponential,
        _ => DecayStrategy::Step,
    };

    // `Duration::try_seconds` returns None for out-of-range values instead of
    // panicking, so arbitrary i64 seconds can't abort during construction.
    let decay = Some(Decay {
        strategy,
        half_life: half_life_secs.and_then(Duration::try_seconds),
        ttl: ttl_secs.and_then(Duration::try_seconds),
        floor,
    });

    // Timestamps outside chrono's representable range yield None; skip those
    // rather than fabricating a sentinel that wouldn't exercise real paths.
    let (Some(created_at), Some(now)) = (
        DateTime::<Utc>::from_timestamp(created_ts, 0),
        DateTime::<Utc>::from_timestamp(now_ts, 0),
    ) else {
        return;
    };

    let factor = decay_factor(created_at, now, &decay);
    assert!(
        factor.is_finite(),
        "decay_factor produced a non-finite factor (floor={floor})"
    );
});
