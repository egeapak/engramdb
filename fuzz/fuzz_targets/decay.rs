#![no_main]

use chrono::{DateTime, Duration, Utc};
use libfuzzer_sys::fuzz_target;

use engramdb::scoring::decay_factor;
use engramdb::types::{Decay, DecayStrategy};

// `decay_factor` does float math over caller-influenced durations: an age in
// seconds divided by a TTL / half-life, then `0.5.powf(..)` and a clamp. TTL,
// half-life, floor and timestamps ultimately come from on-disk memory files,
// so a hostile or corrupt file can drive every input here. The factor
// multiplies into relevance, so it must always land in [0, 1] (a NaN/inf or
// out-of-range factor would unorder search results or break later
// arithmetic), regardless of zero/negative/overflowing durations, future
// timestamps, or a non-finite/out-of-range floor — `decay_factor` clamps the
// floor into [0, 1] itself (NaN → 0.0), so no input is skipped here.
fuzz_target!(|input: (u8, Option<i64>, Option<i64>, f64, i64, i64)| {
    let (strat_sel, ttl_secs, half_life_secs, floor, created_ts, now_ts) = input;

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
        (0.0..=1.0).contains(&factor),
        "decay_factor produced a factor outside [0, 1] (factor={factor}, floor={floor})"
    );
});
