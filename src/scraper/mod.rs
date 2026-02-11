//! Site adapters and scraping. Site detection, scraper trait, shared client, and adapters.

mod client;
mod error;

pub mod royalroad;
pub mod scribblehub;

pub use client::{PoliteClient, PoliteClientBuilder};
pub use error::ScraperError;

use crate::model::Book;
use reqwest::Url;

/// Strip known site suffix from the end of a page title (e.g. " - Royal Road", " | Scribble Hub")
/// so that titles containing " - " or " | " in the actual title are preserved.
pub fn strip_title_site_suffix(s: &str, suffixes: &[&str]) -> String {
    let mut t = s.trim();
    for suffix in suffixes {
        if t.ends_with(suffix) {
            t = t[..t.len() - suffix.len()].trim();
            break;
        }
    }
    t.to_string()
}

/// How to handle Royal Road locked (premium) chapters. Only applies to Royal Road.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedChapterBehavior {
    /// Exclude locked chapters from the TOC (default).
    Skip,
    /// Include a placeholder chapter (title and body indicating locked) for each locked chapter.
    Placeholder,
    /// Fail the scrape if any chapter is locked.
    Fail,
}

/// How to handle chapters with empty body or missing content container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyChapterBehavior {
    /// Skip the chapter (default).
    Skip,
    /// Include a placeholder chapter (title and body indicating no content).
    Placeholder,
    /// Fail the scrape.
    Fail,
}

/// Supported fiction site. Used for dispatch and for --site override (Phase 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    RoyalRoad,
    ScribbleHub,
}

/// Options for a scrape run: progress callback, chapter range, resume state, checkpoint, locked/empty handling, toc-only.
pub struct ScrapeOptions<'a> {
    pub progress: Option<&'a dyn Fn(u32, u32)>,
    pub chapter_range: Option<(u32, u32)>,
    pub initial_book: Option<&'a Book>,
    pub on_checkpoint: Option<&'a dyn Fn(&Book)>,
    pub locked_behavior: Option<LockedChapterBehavior>,
    /// How to handle empty body or missing content container (default Skip).
    pub empty_chapter_behavior: Option<EmptyChapterBehavior>,
    pub toc_only: bool,
}

/// Resolve which site to use from URL and optional override. Messages per ERROR_HANDLING.md 2.2.
pub fn resolve_site(url_input: &str, override_site: Option<Site>) -> Result<Site, ScraperError> {
    if let Some(site) = override_site {
        return Ok(site);
    }
    let url = Url::parse(url_input).map_err(|e| ScraperError::InvalidUrl {
        input: url_input.to_string(),
        reason: e.to_string(),
    })?;
    let host = url.host_str().ok_or_else(|| ScraperError::InvalidUrl {
        input: url_input.to_string(),
        reason: "URL has no host".to_string(),
    })?;
    if host.contains("royalroad.com") {
        Ok(Site::RoyalRoad)
    } else if host.contains("scribblehub.com") {
        Ok(Site::ScribbleHub)
    } else {
        Err(ScraperError::UnrecognizedHost {
            host: host.to_string(),
        })
    }
}

/// Trait implemented by site adapters (Royal Road, Scribble Hub).
///
/// Returns the canonical [Book](crate::model::Book) (shape per OUTPUT_SHAPE.md).
/// See [ScrapeOptions] for the meaning of each option.
pub trait Scraper {
    fn scrape_book(&mut self, url: &str, options: &ScrapeOptions<'_>)
        -> Result<Book, ScraperError>;
}

/// Dispatch by site: build the appropriate adapter and call scrape_book.
pub fn scrape_book(
    site: Site,
    url: &str,
    client: &mut PoliteClient,
    options: &ScrapeOptions<'_>,
) -> Result<Book, ScraperError> {
    match site {
        Site::RoyalRoad => {
            let mut adapter = royalroad::RoyalRoadScraper::new(client);
            adapter.scrape_book(url, options)
        }
        Site::ScribbleHub => {
            let mut adapter = scribblehub::ScribbleHubScraper::new(client);
            adapter.scrape_book(url, options)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_title_site_suffix_removes_trailing_suffix_only() {
        assert_eq!(
            strip_title_site_suffix(
                "Chapter 1 - The Beginning | Scribble Hub",
                &[" | Scribble Hub", " - Scribble Hub"]
            ),
            "Chapter 1 - The Beginning"
        );
        assert_eq!(
            strip_title_site_suffix(
                "Chapter 1 | Part 2 | Scribble Hub",
                &[" | Scribble Hub", " - Scribble Hub"]
            ),
            "Chapter 1 | Part 2"
        );
        assert_eq!(
            strip_title_site_suffix(
                "1. Good Morning - Brother - Book _ Royal Road",
                &[" _ Royal Road", " - Royal Road", " | Royal Road"]
            ),
            "1. Good Morning - Brother - Book"
        );
    }

    #[test]
    fn site_detection_royalroad() -> Result<(), ScraperError> {
        let site = resolve_site("https://www.royalroad.com/fiction/123/slug", None)?;
        assert_eq!(site, Site::RoyalRoad);
        Ok(())
    }

    #[test]
    fn site_detection_scribblehub() -> Result<(), ScraperError> {
        let site = resolve_site("https://www.scribblehub.com/series/1/slug/", None)?;
        assert_eq!(site, Site::ScribbleHub);
        Ok(())
    }

    #[test]
    fn site_detection_unrecognized_host_errors() -> Result<(), String> {
        let result = resolve_site("https://example.com/foo", None);
        match &result {
            Err(ScraperError::UnrecognizedHost { host }) if host == "example.com" => Ok(()),
            _ => Err(format!("expected UnrecognizedHost, got {:?}", result)),
        }
    }

    #[test]
    fn site_detection_invalid_url_errors() -> Result<(), String> {
        let result = resolve_site("not-a-url", None);
        match &result {
            Err(ScraperError::InvalidUrl { input, .. }) if input == "not-a-url" => Ok(()),
            _ => Err(format!("expected InvalidUrl, got {:?}", result)),
        }
    }

    #[test]
    fn site_override_ignores_url_host() -> Result<(), ScraperError> {
        let site = resolve_site("https://example.com/foo", Some(Site::RoyalRoad))?;
        assert_eq!(site, Site::RoyalRoad);
        Ok(())
    }
}
