// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

/// Converts f64 seconds to f64 ms rounded to 3 decimals.
pub fn pretty_secs_to_ms(secs: f64) -> f64 {
    (secs * 1_000_000.0).trunc() / 1_000.0
}

/// Rounds f64 ms to 3 decimals.
pub fn pretty_ms(ms: f64) -> f64 {
    (ms * 1_000.0).trunc() / 1_000.0
}

/// Round f64 seconds to 3 decimals.
pub fn pretty_secs(secs: f64) -> f64 {
    (secs * 1_000.0).trunc() / 1_000.0
}
