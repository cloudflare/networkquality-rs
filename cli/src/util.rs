/// Converts f64 seconds to f64 ms rounded to 3 decimals.
pub fn pretty_secs_to_ms(secs: f64) -> f64 {
    (secs * 1_000_000.0).trunc() / 1_000.0
}

/// Rounds f64 ms to 3 decimals.
pub fn pretty_ms(ms: f64) -> f64 {
    (ms * 1_000.0).trunc() / 1_000.0
}
