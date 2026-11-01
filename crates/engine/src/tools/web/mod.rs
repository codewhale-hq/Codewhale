//! Shared web-tool helpers: the SSRF guard and the search scrapers.
//!
//! `fetch_url`, `web_search`, and `web.run` are thin surfaces over this
//! module so security and parsing behavior cannot drift between tools.

pub mod adapter;
pub mod backend;
pub mod cache;
pub mod citations;
pub mod contract;
pub mod extract;
pub mod fetch;
pub mod guard;
pub mod overflow;
pub mod scrape;
