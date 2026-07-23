// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Cloudflare Access support for testing gated endpoints.
//!
//! When the `_MACH_CF_ACCESS_TOKEN` environment variable is set, mach attaches a
//! `cf-access-token` header to its test requests so that Cloudflare
//! Access-gated hosts can be reached. The token is only ever attached to hosts
//! on the allowlist below so that it is never leaked to an unrelated origin
//! returned by a remote configuration.

use anyhow::Context;
use http::{HeaderMap, HeaderValue};
use nq_core::ScopedHeaders;

/// Environment variable holding the Cloudflare Access token.
const ACCESS_TOKEN_ENV: &str = "_MACH_CF_ACCESS_TOKEN";

/// Header used to carry the Cloudflare Access token.
const ACCESS_TOKEN_HEADER: &str = "cf-access-token";

/// Domain suffixes the access token may be sent to. Matching is done with
/// strict label boundaries by [`ScopedHeaders`].
const ALLOWED_HOST_SUFFIXES: &[&str] = &["speed.cloudflare.com"];

/// Build the scoped headers carrying the Cloudflare Access token, if the
/// `_MACH_CF_ACCESS_TOKEN` environment variable is set to a non-empty value.
///
/// Returns `Ok(None)` when the variable is unset or blank. Returns an error
/// when the token is not a valid header value, so a misconfigured token fails
/// fast at startup rather than silently sending no header.
pub fn cf_access_scoped_headers() -> anyhow::Result<Option<ScopedHeaders>> {
    let Ok(token) = std::env::var(ACCESS_TOKEN_ENV) else {
        return Ok(None);
    };

    let token = token.trim();
    if token.is_empty() {
        return Ok(None);
    }

    let mut value = HeaderValue::from_str(token)
        .with_context(|| format!("{ACCESS_TOKEN_ENV} is not a valid header value"))?;
    // Mark the value sensitive so it is redacted (printed as `Sensitive`) in
    // request/response debug logs.
    value.set_sensitive(true);

    let mut headers = HeaderMap::new();
    headers.insert(ACCESS_TOKEN_HEADER, value);

    let allowed_host_suffixes = ALLOWED_HOST_SUFFIXES
        .iter()
        .map(|s| s.to_string())
        .collect();

    Ok(Some(ScopedHeaders::new(headers, allowed_host_suffixes)))
}
