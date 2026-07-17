// Copyright (c) 2023-2024 Cloudflare, Inc.
// Licensed under the BSD-3-Clause license found in the LICENSE file or at https://opensource.org/licenses/BSD-3-Clause

//! Headers that are only attached to requests whose host matches an allowlist.
//!
//! This is a generic mechanism: callers supply the headers and the list of
//! allowed domain suffixes. It is used to attach sensitive headers (such as an
//! access token) to trusted hosts only, so the header is never sent to an
//! unrelated origin returned by a remote configuration.

use http::{HeaderMap, Uri};

/// A set of headers attached to a request only when the request's host matches
/// one of the allowed domain suffixes.
///
/// Matching uses strict label boundaries: a host matches a suffix when it is
/// equal to the suffix or ends with `.` followed by the suffix. This prevents
/// look-alike hosts such as `example.com.evil.com` from matching `example.com`.
///
/// Existing headers are never overwritten, so a header set explicitly by the
/// caller takes precedence over a scoped header with the same name.
#[derive(Debug, Clone, Default)]
pub struct ScopedHeaders {
    headers: HeaderMap,
    allowed_host_suffixes: Vec<String>,
}

impl ScopedHeaders {
    /// Create a new [`ScopedHeaders`] from the headers to attach and the list
    /// of allowed domain suffixes.
    ///
    /// Suffixes are normalized (lowercased and stripped of any trailing dot) so
    /// matching in [`allows_host`](Self::allows_host) is case-insensitive and
    /// tolerant of fully-qualified (trailing-dot) hostnames.
    pub fn new(headers: HeaderMap, allowed_host_suffixes: Vec<String>) -> Self {
        let allowed_host_suffixes = allowed_host_suffixes
            .into_iter()
            .map(|s| s.trim_end_matches('.').to_ascii_lowercase())
            .collect();

        Self {
            headers,
            allowed_host_suffixes,
        }
    }

    /// Returns whether the given host is covered by an allowed suffix, using
    /// strict label-boundary matching.
    ///
    /// Matching is case-insensitive (DNS hostnames are case-insensitive) and
    /// ignores a trailing dot on the host (fully-qualified domain names).
    pub fn allows_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.allowed_host_suffixes.iter().any(|suffix| {
            host == *suffix
                || (host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
        })
    }

    /// Attach the scoped headers to `headers` when the URI's host matches an
    /// allowed suffix. Headers already present in `headers` are left untouched.
    pub fn apply(&self, uri: &Uri, headers: &mut HeaderMap) {
        let Some(host) = uri.host() else {
            return;
        };

        if !self.allows_host(host) {
            return;
        }

        for (name, value) in self.headers.iter() {
            if !headers.contains_key(name) {
                headers.append(name, value.clone());
            }
        }
    }
}
