//! Shared error type for scrapers. Messages align with ERROR_HANDLING.md 2.2 and 2.3.

use thiserror::Error;

/// Shared scraper error for site detection, HTTP, parsing, and site-specific cases.
#[derive(Debug, Error)]
pub enum ScraperError {
    // Site / URL (ERROR_HANDLING 2.2)
    #[error("Invalid URL: {input}: {reason}")]
    InvalidUrl { input: String, reason: String },

    #[error(
        "Could not detect site from URL host '{host}'. Use --site royalroad or --site scribblehub."
    )]
    UnrecognizedHost { host: String },

    // HTTP and network (2.3.1)
    #[error("Network error: could not reach {url}: {source}")]
    Network { url: String, source: reqwest::Error },

    #[error("HTTP {status} when fetching: {url}")]
    HttpStatus {
        status: u16,
        url: String,
        /// Optional context (e.g. "story page", "chapter 5") for programmatic use.
        context: Option<String>,
    },

    #[error("Redirect error: {url}: {reason}")]
    Redirect { url: String, reason: String },

    #[error("TLS error: {source}")]
    Tls { source: reqwest::Error },

    #[error("Failed to read response body: {source}")]
    BodyRead { source: reqwest::Error },

    // Parsing (2.3.2)
    #[error("Could not parse story page: {message}")]
    ParseStoryPage { message: String },

    #[error("Could not parse chapter {index}: missing content container at {url}.")]
    ParseChapter { index: u32, url: String },

    #[error("Chapter {index} has no content at {url}.")]
    EmptyChapter { index: u32, url: String },

    #[error("Invalid encoding or HTML at {url}: {reason}")]
    Encoding { url: String, reason: String },

    #[error("Could not parse chapter list on story page: {reason}")]
    ChapterListParse { reason: String },

    #[error("Story page has no chapters (possibly deleted or access restricted).")]
    EmptyChapterList,

    // Site-specific (2.3.3)
    #[error("Access blocked or restricted at {url}. If using a browser you may need cookies or captcha; scripted access may be limited.")]
    AccessBlocked { url: String },

    #[error("No chapters could be retrieved (all locked, missing, or failed).")]
    NoChaptersRetrieved,

    /// Royal Road: fiction has locked (premium) chapters and --locked-chapters=fail.
    #[error("Fiction has {count} locked (premium) chapter(s). Use --locked-chapters skip or placeholder to include only free chapters or add placeholders.")]
    LockedChaptersNotAllowed { count: usize },
}
