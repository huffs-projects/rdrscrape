//! rdrscrape: CLI scraper for Royal Road and Scribble Hub fiction, outputting EPUB.

pub mod cli;
pub mod config;
pub mod epub;
pub mod formats;
pub mod model;
pub mod scraper;

// Re-exports for CLI and consumers.
pub use epub::{write_epub, EpubError, EpubVersion};
pub use formats::{write_html, write_markdown, write_text, FormatError, OutputFormat};
pub use scraper::{
    resolve_site, scrape_book, EmptyChapterBehavior, PoliteClient, PoliteClientBuilder,
    ScrapeOptions, Scraper, ScraperError, Site,
};
