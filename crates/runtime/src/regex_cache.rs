//! Compiled-regex cache for patterns a user or model supplies (tool search,
//! purge). Those patterns are untrusted input, so compilation is bounded:
//! the pattern length is capped, the compiled program and lazy DFA have
//! explicit size limits, compilation runs outside the cache lock, and a
//! rejected pattern is remembered so repeating it costs a lookup.

use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};

use lru::LruCache;
use regex::{Regex, RegexBuilder};

const DEFAULT_USER_REGEX_CACHE_CAPACITY: usize = 64;
/// Longest pattern accepted, in bytes. Search and purge patterns are short.
pub const MAX_USER_REGEX_PATTERN_BYTES: usize = 4 * 1024;
/// Compiled program limit (the `regex` default is 10 MiB).
const USER_REGEX_SIZE_LIMIT: usize = 1024 * 1024;
/// Lazy DFA cache limit per matcher.
const USER_REGEX_DFA_SIZE_LIMIT: usize = 1024 * 1024;

static USER_REGEX_CACHE: OnceLock<UserRegexCache> = OnceLock::new();

pub fn compile_user_regex(pattern: &str) -> Result<Regex, regex::Error> {
    user_regex_cache().compile(pattern, false)
}

pub fn compile_user_regex_case_insensitive(pattern: &str) -> Result<Regex, regex::Error> {
    user_regex_cache().compile(pattern, true)
}

fn user_regex_cache() -> &'static UserRegexCache {
    USER_REGEX_CACHE.get_or_init(UserRegexCache::new)
}

fn compile_bounded(pattern: &str, case_insensitive: bool) -> Result<Regex, regex::Error> {
    if pattern.len() > MAX_USER_REGEX_PATTERN_BYTES {
        return Err(regex::Error::Syntax(format!(
            "pattern is {} bytes; the limit is {MAX_USER_REGEX_PATTERN_BYTES}",
            pattern.len()
        )));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .size_limit(USER_REGEX_SIZE_LIMIT)
        .dfa_size_limit(USER_REGEX_DFA_SIZE_LIMIT)
        .build()
}

struct UserRegexCache {
    /// Compiled patterns and rejected ones alike, so a repeated bad pattern
    /// is not recompiled.
    inner: Mutex<LruCache<(String, bool), Result<Regex, regex::Error>>>,
}

impl UserRegexCache {
    fn new() -> Self {
        Self::with_capacity(
            NonZeroUsize::new(DEFAULT_USER_REGEX_CACHE_CAPACITY).expect("non-zero capacity"),
        )
    }

    fn with_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    fn compile(&self, pattern: &str, case_insensitive: bool) -> Result<Regex, regex::Error> {
        // An over-long pattern is refused before it is hashed or stored.
        if pattern.len() > MAX_USER_REGEX_PATTERN_BYTES {
            return compile_bounded(pattern, case_insensitive);
        }
        let key = (pattern.to_string(), case_insensitive);
        if let Ok(mut cache) = self.inner.lock()
            && let Some(cached) = cache.get(&key)
        {
            return cached.clone();
        }
        // Compile without the lock: one slow pattern must not stall every
        // other caller. Two racing misses both compile; the result is equal.
        let compiled = compile_bounded(pattern, case_insensitive);
        if let Ok(mut cache) = self.inner.lock() {
            cache.put(key, compiled.clone());
        }
        compiled
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("cache lock").len()
    }

    #[cfg(test)]
    fn contains(&self, pattern: &str) -> bool {
        self.inner
            .lock()
            .expect("cache lock")
            .contains(&(pattern.to_string(), false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_pattern_uses_one_cache_entry() {
        let cache = UserRegexCache::with_capacity(NonZeroUsize::new(2).unwrap());

        let first = cache.compile("alpha|beta", false).expect("regex compiles");
        let second = cache.compile("alpha|beta", false).expect("regex cache hit");

        assert!(first.is_match("alpha"));
        assert!(second.is_match("beta"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn capacity_evicts_least_recently_used_pattern() {
        let cache = UserRegexCache::with_capacity(NonZeroUsize::new(2).unwrap());

        cache.compile("one", false).expect("one compiles");
        cache.compile("two", false).expect("two compiles");
        cache.compile("one", false).expect("one is refreshed");
        cache.compile("three", false).expect("three compiles");

        assert!(cache.contains("one"));
        assert!(!cache.contains("two"));
        assert!(cache.contains("three"));
    }

    #[test]
    fn invalid_pattern_is_remembered_as_invalid() {
        let cache = UserRegexCache::with_capacity(NonZeroUsize::new(2).unwrap());

        assert!(cache.compile("[", false).is_err());
        assert!(cache.contains("["));
        assert!(cache.compile("[", false).is_err());

        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn oversized_patterns_are_refused_without_being_cached() {
        let cache = UserRegexCache::with_capacity(NonZeroUsize::new(2).unwrap());

        let long = "a".repeat(MAX_USER_REGEX_PATTERN_BYTES + 1);
        assert!(cache.compile(&long, false).is_err());
        assert_eq!(cache.len(), 0);

        // Within the length cap, a pattern whose compiled form exceeds the
        // program limit is refused rather than built.
        let explosive = r"(?:\w{100}){100}";
        assert!(explosive.len() <= MAX_USER_REGEX_PATTERN_BYTES);
        assert!(matches!(
            cache.compile(explosive, false),
            Err(regex::Error::CompiledTooBig(_))
        ));
    }
}
