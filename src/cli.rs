//! CLI parsing and orchestration. Parses args, runs scrape -> EPUB, JSON, HTML, Markdown, or text. Maps errors to exit codes.

use crate::config;
use crate::epub::{write_epub, EpubError, EpubVersion};
use crate::formats::{write_html, write_markdown, write_text, FormatError, OutputFormat};
use crate::model::Book;
use crate::scraper::{
    resolve_site, scrape_book, EmptyChapterBehavior, LockedChapterBehavior, ScrapeOptions,
    ScraperError, Site,
};
use crate::PoliteClient;
use clap::Parser;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// CLI error carrying exit code and message. Per ERROR_HANDLING.md 2.1.
#[derive(Debug, Error)]
pub enum CliRunError {
    #[error("{0}")]
    InvalidInput(String),

    #[error("{0}")]
    Scraper(#[from] ScraperError),

    #[error("{0}")]
    Epub(#[from] EpubError),

    #[error("{0}")]
    Format(#[from] FormatError),

    #[error("{0}")]
    Validation(String),
}

impl CliRunError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliRunError::InvalidInput(_) => 1,
            CliRunError::Scraper(_) => 2,
            CliRunError::Epub(_) | CliRunError::Format(_) | CliRunError::Validation(_) => 3,
        }
    }
}

/// Run epubcheck on the given EPUB path. Requires epubcheck on PATH.
fn validate_epub(path: &PathBuf) -> Result<(), CliRunError> {
    let output = std::process::Command::new("epubcheck")
        .arg(path)
        .output()
        .map_err(|e| {
            CliRunError::Validation(format!(
                "Could not run epubcheck: {}. Is epubcheck installed and on PATH?",
                e
            ))
        })?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = if stderr.is_empty() { stdout } else { stderr };
        Err(CliRunError::Validation(format!(
            "epubcheck reported errors:\n{}",
            msg.trim()
        )))
    }
}

#[derive(Parser, Debug)]
#[command(name = "rdrscrape")]
#[command(about = "Scrape Royal Road or Scribble Hub fiction and write EPUB")]
#[command(
    after_help = "Config file keys (output_dir, user_agent, request_delay_secs, timeout_secs, toc_page, retry_count, retry_backoff_secs, empty_chapters) are documented in the README. CLI flags override config."
)]
pub struct Args {
    /// Story or series URL (Royal Road fiction page or Scribble Hub series page).
    pub url: String,

    /// Output path. Default: ./{sanitized-title}.{ext} where ext depends on --format.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: epub, json, html, markdown, or text.
    #[arg(long, default_value = "epub", value_parser = parse_format)]
    pub format: OutputFormat,

    /// Override site detection (royalroad or scribblehub).
    #[arg(long, value_parser = parse_site)]
    pub site: Option<Site>,

    /// Generate EPUB 2 instead of EPUB 3 (only when format is epub).
    #[arg(long)]
    pub epub_2: bool,

    /// Suppress progress output (errors only).
    #[arg(short, long)]
    pub quiet: bool,

    /// Print verbose error chain.
    #[arg(long)]
    pub verbose: bool,

    /// Include toc.ncx in EPUB 3 output for legacy readers (no effect for EPUB 2, which always includes NCX).
    #[arg(long)]
    pub ncx: bool,

    /// Scrape only chapters in this range (1-based inclusive), e.g. 1-10 or 5-20.
    #[arg(long, value_parser = parse_chapter_range)]
    pub chapters: Option<(u32, u32)>,

    /// Resume from a partial scrape saved at this path (JSON). Load existing chapters and fetch only missing ones; save progress after each chapter.
    #[arg(long)]
    pub resume: Option<PathBuf>,

    /// How to handle Royal Road locked (premium) chapters: skip (default), placeholder, or fail.
    #[arg(long, default_value = "skip", value_parser = parse_locked_behavior)]
    pub locked_chapters: LockedChapterBehavior,

    /// How to handle chapters with empty body or missing content: skip (default), placeholder, or fail.
    #[arg(long, value_parser = parse_empty_chapter_behavior)]
    pub empty_chapters: Option<EmptyChapterBehavior>,

    /// HTTP User-Agent (overrides config).
    #[arg(long)]
    pub user_agent: Option<String>,

    /// Delay between requests in seconds (overrides config; default 2).
    #[arg(long)]
    pub delay: Option<u64>,

    /// Request timeout in seconds (overrides config; default 30).
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Resolve site, fetch TOC only, print chapter count and output path without writing.
    #[arg(long)]
    pub dry_run: bool,

    /// After writing an EPUB, run epubcheck to validate it (epubcheck must be on PATH). No effect for non-EPUB output.
    #[arg(long)]
    pub validate: bool,
}

fn parse_chapter_range(s: &str) -> Result<(u32, u32), String> {
    let s = s.trim();
    let (from_str, to_str) = s.split_once('-').ok_or_else(|| {
        format!(
            "Invalid --chapters: expected 'from-to' (e.g. 1-10), got '{}'",
            s
        )
    })?;
    let from_str = from_str.trim();
    let to_str = to_str.trim();
    let from: u32 = from_str.parse().map_err(|_| {
        format!(
            "Invalid --chapters: '{}' is not a valid start chapter number",
            from_str
        )
    })?;
    let to: u32 = to_str.parse().map_err(|_| {
        format!(
            "Invalid --chapters: '{}' is not a valid end chapter number",
            to_str
        )
    })?;
    if from > to {
        return Err(format!(
            "Invalid --chapters: start ({}). must be <= end ({})",
            from, to
        ));
    }
    Ok((from, to))
}

fn parse_site(s: &str) -> Result<Site, String> {
    match s.to_lowercase().as_str() {
        "royalroad" | "rr" => Ok(Site::RoyalRoad),
        "scribblehub" | "sh" => Ok(Site::ScribbleHub),
        _ => Err(format!(
            "Invalid --site value: '{}'. Use 'royalroad' or 'scribblehub'.",
            s
        )),
    }
}

fn parse_locked_behavior(s: &str) -> Result<LockedChapterBehavior, String> {
    match s.to_lowercase().as_str() {
        "skip" => Ok(LockedChapterBehavior::Skip),
        "placeholder" => Ok(LockedChapterBehavior::Placeholder),
        "fail" => Ok(LockedChapterBehavior::Fail),
        _ => Err(format!(
            "Invalid --locked-chapters value: '{}'. Use skip, placeholder, or fail.",
            s
        )),
    }
}

fn parse_empty_chapter_behavior(s: &str) -> Result<EmptyChapterBehavior, String> {
    match s.to_lowercase().as_str() {
        "skip" => Ok(EmptyChapterBehavior::Skip),
        "placeholder" => Ok(EmptyChapterBehavior::Placeholder),
        "fail" => Ok(EmptyChapterBehavior::Fail),
        _ => Err(format!(
            "Invalid --empty-chapters value: '{}'. Use skip, placeholder, or fail.",
            s
        )),
    }
}

fn parse_format(s: &str) -> Result<OutputFormat, String> {
    match s.to_lowercase().as_str() {
        "epub" => Ok(OutputFormat::Epub),
        "json" => Ok(OutputFormat::Json),
        "html" => Ok(OutputFormat::Html),
        "markdown" | "md" => Ok(OutputFormat::Markdown),
        "text" | "txt" => Ok(OutputFormat::Text),
        _ => Err(format!(
            "Invalid --format value: '{}'. Use epub, json, html, markdown, or text.",
            s
        )),
    }
}

fn extension_for_format(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Epub => "epub",
        OutputFormat::Json => "json",
        OutputFormat::Html => "html",
        OutputFormat::Markdown => "md",
        OutputFormat::Text => "txt",
    }
}

/// Sanitize book title to a safe filename: lowercase, replace spaces/special with `-`.
fn sanitize_title(title: &str) -> String {
    let mut s = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    // Collapse multiple dashes and trim
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s = s.trim_matches('-').to_string();
    if s.is_empty() {
        s = "book".to_string();
    }
    s
}

/// Ensure output path parent exists and is writable; return path.
fn validate_output_path(path: &Path) -> Result<(), CliRunError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(CliRunError::InvalidInput(format!(
                "Cannot write output: {}: parent directory does not exist.",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Entry point for the CLI. Returns Ok(()) on success; Err with exit code and message on failure.
pub fn run(args: &Args) -> Result<(), CliRunError> {
    let site = resolve_site(&args.url, args.site).map_err(|e| match &e {
        ScraperError::InvalidUrl { input, reason } => CliRunError::InvalidInput(format!(
            "Expected a story URL. Example: https://www.royalroad.com/fiction/12345/... Invalid: {}: {}",
            input, reason
        )),
        ScraperError::UnrecognizedHost { host } => CliRunError::InvalidInput(format!(
            "Unsupported site: {}. Use --site royalroad or scribblehub to override, or provide a Royal Road / Scribble Hub URL.",
            host
        )),
        _ => CliRunError::Scraper(e),
    })?;

    let config = config::load_config().map_err(CliRunError::InvalidInput)?;

    let effective_output_dir: PathBuf = config
        .as_ref()
        .and_then(|c| c.output_dir.clone())
        .unwrap_or_else(|| PathBuf::from("."));

    const DEFAULT_DELAY_SECS: u64 = 2;
    const DEFAULT_TIMEOUT_SECS: u64 = 30;
    const DEFAULT_RETRY_COUNT: u32 = 3;
    let delay_secs = args
        .delay
        .or_else(|| config.as_ref().and_then(|c| c.request_delay_secs))
        .unwrap_or(DEFAULT_DELAY_SECS);
    let timeout_secs = args
        .timeout
        .or_else(|| config.as_ref().and_then(|c| c.timeout_secs))
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let retry_count = config
        .as_ref()
        .and_then(|c| c.retry_count)
        .unwrap_or(DEFAULT_RETRY_COUNT)
        .max(1);
    let retry_backoff_secs = config
        .as_ref()
        .and_then(|c| c.retry_backoff_secs.clone())
        .unwrap_or_else(|| vec![1, 2, 4]);
    let user_agent = args
        .user_agent
        .clone()
        .or_else(|| config.as_ref().and_then(|c| c.user_agent.clone()));

    let mut builder = PoliteClient::builder()
        .delay_secs(delay_secs)
        .timeout_secs(timeout_secs)
        .retry_count(retry_count)
        .retry_backoff_secs(retry_backoff_secs);
    if let Some(ua) = user_agent {
        builder = builder.user_agent(ua);
    }
    let mut client = builder
        .build()
        .map_err(|e| CliRunError::InvalidInput(format!("Failed to create HTTP client: {}", e)))?;

    let progress_state: RefCell<Option<indicatif::ProgressBar>> = RefCell::new(None);
    let progress_cb = |n: u32, total: u32| {
        if total == 0 {
            return;
        }
        let mut state = progress_state.borrow_mut();
        let pb = state.get_or_insert_with(|| {
            let bar = indicatif::ProgressBar::new(total as u64);
            bar.set_style(
                indicatif::ProgressStyle::default_bar()
                    .template("{spinner} {msg} [{bar:40}] {pos}/{len} ({elapsed})")
                    .unwrap()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                    .progress_chars("█▉▊▋▌▍▎▏ "),
            );
            bar.enable_steady_tick(Duration::from_millis(80));
            bar
        });
        pb.set_position(n as u64);
        pb.set_message(format!("Fetching chapter {}/{}", n, total));
    };
    let progress: Option<&dyn Fn(u32, u32)> = if args.quiet { None } else { Some(&progress_cb) };

    let initial_book: Option<Book> = if let Some(ref resume_path) = args.resume {
        match std::fs::File::open(resume_path) {
            Ok(f) => {
                let loaded: Book = serde_json::from_reader(f).map_err(|e| {
                    CliRunError::InvalidInput(format!(
                        "Invalid resume file {}: {}",
                        resume_path.display(),
                        e
                    ))
                })?;
                if let Some(ref surl) = loaded.source_url {
                    let a = surl.trim_end_matches('/');
                    let b = args.url.trim_end_matches('/');
                    if a != b {
                        return Err(CliRunError::InvalidInput(format!(
                            "Resume file is for a different URL ({}). Use the same URL as the original run ({}).",
                            surl, args.url
                        )));
                    }
                }
                Some(loaded)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(CliRunError::InvalidInput(format!(
                    "Cannot read resume file {}: {}",
                    resume_path.display(),
                    e
                )))
            }
        }
    } else {
        None
    };
    let initial_book_ref = initial_book.as_ref();

    let resume_path = args.resume.clone();
    let checkpoint_cb = |book: &Book| {
        if let Some(ref path) = resume_path {
            if let Err(e) = std::fs::File::create(path).and_then(|f| {
                serde_json::to_writer(f, book)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }) {
                eprintln!(
                    "Warning: could not write resume file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    };
    let on_checkpoint: Option<&dyn Fn(&Book)> = if args.resume.is_some() {
        Some(&checkpoint_cb)
    } else {
        None
    };

    let empty_chapter_behavior = args
        .empty_chapters
        .or_else(|| {
            config
                .as_ref()
                .and_then(|c| c.empty_chapters.as_deref())
                .and_then(|s| parse_empty_chapter_behavior(s).ok())
        })
        .unwrap_or(EmptyChapterBehavior::Skip);

    if args.dry_run {
        let dry_run_opts = ScrapeOptions {
            progress: None,
            chapter_range: args.chapters,
            initial_book: None,
            on_checkpoint: None,
            locked_behavior: Some(args.locked_chapters),
            empty_chapter_behavior: Some(empty_chapter_behavior),
            toc_only: true,
        };
        let book = scrape_book(site, &args.url, &mut client, &dry_run_opts)?;
        let output_path = match &args.output {
            Some(p) => p.clone(),
            None => {
                let base = sanitize_title(&book.title);
                let ext = extension_for_format(args.format);
                effective_output_dir.join(format!("{}.{}", base, ext))
            }
        };
        eprintln!("Chapters: {}", book.chapters.len());
        eprintln!("Output: {}", output_path.display());
        return Ok(());
    }

    let scrape_opts = ScrapeOptions {
        progress,
        chapter_range: args.chapters,
        initial_book: initial_book_ref,
        on_checkpoint,
        locked_behavior: Some(args.locked_chapters),
        empty_chapter_behavior: Some(empty_chapter_behavior),
        toc_only: false,
    };
    let book = scrape_book(site, &args.url, &mut client, &scrape_opts)?;

    if let Some(pb) = progress_state.borrow_mut().take() {
        pb.disable_steady_tick();
        pb.finish_and_clear();
    }

    let output_path = match &args.output {
        Some(p) => p.clone(),
        None => {
            let base = sanitize_title(&book.title);
            let ext = extension_for_format(args.format);
            effective_output_dir.join(format!("{}.{}", base, ext))
        }
    };

    validate_output_path(&output_path)?;

    match args.format {
        OutputFormat::Json => {
            let f = std::fs::File::create(&output_path).map_err(|e| {
                CliRunError::Epub(EpubError::CreateFile {
                    path: output_path.clone(),
                    source: e,
                })
            })?;
            serde_json::to_writer(f, &book)
                .map_err(|e| CliRunError::InvalidInput(format!("Failed to write JSON: {}", e)))?;
        }
        OutputFormat::Epub => {
            let version = if args.epub_2 {
                EpubVersion::Epub2
            } else {
                EpubVersion::Epub3
            };
            let include_toc_page = config.as_ref().and_then(|c| c.toc_page).unwrap_or(true);
            write_epub(
                &book,
                &output_path,
                version,
                args.ncx,
                include_toc_page,
                &mut client,
            )?;
            if args.validate {
                validate_epub(&output_path)?;
            }
        }
        OutputFormat::Html => write_html(&book, &output_path)?,
        OutputFormat::Markdown => write_markdown(&book, &output_path)?,
        OutputFormat::Text => write_text(&book, &output_path)?,
    }

    if !args.quiet {
        eprintln!("Wrote {}", output_path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_title_empty() {
        assert_eq!(sanitize_title(""), "book");
    }

    #[test]
    fn sanitize_title_spaces_and_special_to_dashes() {
        assert_eq!(sanitize_title("My  Story!"), "my-story");
    }

    #[test]
    fn sanitize_title_collapse_dashes_and_trim() {
        assert_eq!(sanitize_title("  --  a  --  b  --  "), "a-b");
    }

    #[test]
    fn sanitize_title_alphanumeric_lowercased() {
        assert_eq!(sanitize_title("Mother of Learning"), "mother-of-learning");
    }

    #[test]
    fn parse_chapter_range_valid() {
        assert_eq!(parse_chapter_range("1-10").unwrap(), (1, 10));
        assert_eq!(parse_chapter_range("5-5").unwrap(), (5, 5));
        assert_eq!(parse_chapter_range("  3 - 7  ").unwrap(), (3, 7));
    }

    #[test]
    fn parse_chapter_range_rejects_no_dash() {
        assert!(parse_chapter_range("1").is_err());
    }

    #[test]
    fn parse_chapter_range_rejects_non_numeric() {
        assert!(parse_chapter_range("a-b").is_err());
        assert!(parse_chapter_range("1-b").is_err());
    }

    #[test]
    fn parse_chapter_range_rejects_from_gt_to() {
        assert!(parse_chapter_range("10-1").is_err());
    }

    #[test]
    fn default_output_path_uses_output_dir_and_sanitized_title() {
        let output_dir = PathBuf::from("out");
        let base = sanitize_title("My Book");
        let ext = extension_for_format(OutputFormat::Epub);
        let path = output_dir.join(format!("{}.{}", base, ext));
        assert_eq!(path, PathBuf::from("out/my-book.epub"));
    }

    #[test]
    fn parse_site_royalroad() {
        assert_eq!(parse_site("royalroad").unwrap(), Site::RoyalRoad);
        assert_eq!(parse_site("rr").unwrap(), Site::RoyalRoad);
        assert_eq!(parse_site("RoyalRoad").unwrap(), Site::RoyalRoad);
    }

    #[test]
    fn parse_site_scribblehub() {
        assert_eq!(parse_site("scribblehub").unwrap(), Site::ScribbleHub);
        assert_eq!(parse_site("sh").unwrap(), Site::ScribbleHub);
    }

    #[test]
    fn parse_site_invalid() {
        assert!(parse_site("other").is_err());
    }

    #[test]
    fn parse_format_all() {
        assert_eq!(parse_format("epub").unwrap(), OutputFormat::Epub);
        assert_eq!(parse_format("json").unwrap(), OutputFormat::Json);
        assert_eq!(parse_format("html").unwrap(), OutputFormat::Html);
        assert_eq!(parse_format("markdown").unwrap(), OutputFormat::Markdown);
        assert_eq!(parse_format("md").unwrap(), OutputFormat::Markdown);
        assert_eq!(parse_format("text").unwrap(), OutputFormat::Text);
        assert_eq!(parse_format("txt").unwrap(), OutputFormat::Text);
        assert_eq!(parse_format("EPUB").unwrap(), OutputFormat::Epub);
    }

    #[test]
    fn parse_format_invalid() {
        assert!(parse_format("pdf").is_err());
    }

    #[test]
    fn parse_locked_behavior_all() {
        assert_eq!(
            parse_locked_behavior("skip").unwrap(),
            LockedChapterBehavior::Skip
        );
        assert_eq!(
            parse_locked_behavior("placeholder").unwrap(),
            LockedChapterBehavior::Placeholder
        );
        assert_eq!(
            parse_locked_behavior("fail").unwrap(),
            LockedChapterBehavior::Fail
        );
        assert_eq!(
            parse_locked_behavior("SKIP").unwrap(),
            LockedChapterBehavior::Skip
        );
        assert!(parse_locked_behavior("other").is_err());
    }

    #[test]
    fn extension_for_format_each() {
        assert_eq!(extension_for_format(OutputFormat::Epub), "epub");
        assert_eq!(extension_for_format(OutputFormat::Json), "json");
        assert_eq!(extension_for_format(OutputFormat::Html), "html");
        assert_eq!(extension_for_format(OutputFormat::Markdown), "md");
        assert_eq!(extension_for_format(OutputFormat::Text), "txt");
    }

    #[test]
    fn validate_output_path_parent_exists() {
        let path = std::env::temp_dir().join("rdrscrape_cli_test_output.epub");
        assert!(validate_output_path(&path).is_ok());
    }

    #[test]
    fn validate_output_path_parent_missing() {
        let path = PathBuf::from("/nonexistent_dir_rdrscrape_xyz/output.epub");
        let result = validate_output_path(&path);
        assert!(result.is_err());
        if let Err(CliRunError::InvalidInput(msg)) = result {
            assert!(msg.contains("parent directory does not exist"));
        }
    }

    #[test]
    fn cli_run_error_exit_codes() {
        assert_eq!(CliRunError::InvalidInput("x".into()).exit_code(), 1);
        assert_eq!(
            CliRunError::Scraper(ScraperError::UnrecognizedHost { host: "x".into() }).exit_code(),
            2
        );
        assert_eq!(CliRunError::Epub(EpubError::EmptyTitle).exit_code(), 3);
        assert_eq!(CliRunError::Format(FormatError::EmptyAuthor).exit_code(), 3);
        assert_eq!(
            CliRunError::Validation("epubcheck failed".into()).exit_code(),
            3
        );
    }
}
