//! Single-file output formats: HTML, Markdown, and plain text.
//! Consumes the canonical Book and writes one file per format.

use crate::model::Book;
use scraper::Html;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use thiserror::Error;

/// Output format selector for the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Epub,
    Json,
    Html,
    Markdown,
    Text,
}

/// Errors from the format writers (HTML, Markdown, text).
#[derive(Debug, Error)]
pub enum FormatError {
    #[error("Cannot write: book title is empty.")]
    EmptyTitle,

    #[error("Cannot write: book author is empty.")]
    EmptyAuthor,

    #[error("Failed to write output: {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write output: {0}")]
    Write(#[from] std::io::Error),
}

fn validate_book(book: &Book) -> Result<(), FormatError> {
    if book.title.trim().is_empty() {
        return Err(FormatError::EmptyTitle);
    }
    if book.author.trim().is_empty() {
        return Err(FormatError::EmptyAuthor);
    }
    Ok(())
}

pub(crate) fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Strip HTML from chapter body to plain text using scraper.
pub(crate) fn body_to_plain_text(body: &str) -> String {
    let fragment = Html::parse_fragment(body);
    let root = fragment.root_element();
    let text: String = root.text().collect();
    if text.trim().is_empty() {
        body.to_string().trim().to_string()
    } else {
        text.trim().to_string()
    }
}

/// Write a single HTML file with full book: title, author, description, and all chapters.
pub fn write_html(book: &Book, path: &Path) -> Result<(), FormatError> {
    validate_book(book)?;

    let path = path.to_path_buf();
    let mut f = File::create(&path).map_err(|e| FormatError::Io {
        path: path.clone(),
        source: e,
    })?;

    let title_esc = html_escape_attr(&book.title);
    let author_esc = html_escape_attr(&book.author);
    let description_esc = book
        .description
        .as_deref()
        .map(html_escape_attr)
        .unwrap_or_default();

    writeln!(f, r#"<!DOCTYPE html>"#)?;
    writeln!(f, r#"<html lang="en">"#)?;
    writeln!(f, r#"<head>"#)?;
    writeln!(f, r#"  <meta charset="UTF-8"/>"#)?;
    writeln!(f, r#"  <title>{}</title>"#, title_esc)?;
    writeln!(f, r#"</head>"#)?;
    writeln!(f, r#"<body>"#)?;
    writeln!(f, r#"  <header>"#)?;
    writeln!(f, r#"    <h1>{}</h1>"#, title_esc)?;
    writeln!(f, r#"    <p class="author">By {}</p>"#, author_esc)?;
    if !description_esc.is_empty() {
        writeln!(f, r#"    <p class="description">{}</p>"#, description_esc)?;
    }
    writeln!(f, r#"  </header>"#)?;

    for ch in &book.chapters {
        let ch_title_esc = html_escape_attr(&ch.title);
        writeln!(f, r#"  <section class="chapter">"#)?;
        writeln!(f, r#"    <h2>{}</h2>"#, ch_title_esc)?;
        writeln!(f, r#"    <div class="chapter-body">"#)?;
        f.write_all(ch.body.as_bytes())?;
        writeln!(f)?;
        writeln!(f, r#"    </div>"#)?;
        writeln!(f, r#"  </section>"#)?;
    }

    writeln!(f, r#"</body>"#)?;
    writeln!(f, r#"</html>"#)?;

    Ok(())
}

/// Write a single Markdown file: title, author, description, then each chapter as ## title + body (HTML converted to Markdown).
pub fn write_markdown(book: &Book, path: &Path) -> Result<(), FormatError> {
    validate_book(book)?;

    let path = path.to_path_buf();
    let mut f = File::create(&path).map_err(|e| FormatError::Io {
        path: path.clone(),
        source: e,
    })?;

    writeln!(f, "# {}", book.title)?;
    writeln!(f)?;
    writeln!(f, "By {}", book.author)?;
    writeln!(f)?;
    if let Some(ref d) = book.description {
        writeln!(f, "{}", d)?;
        writeln!(f)?;
    }
    writeln!(f, "---")?;
    writeln!(f)?;

    for ch in &book.chapters {
        writeln!(f, "## {}", ch.title)?;
        writeln!(f)?;
        let md = html2md::parse_html(&ch.body);
        writeln!(f, "{}", md)?;
        writeln!(f)?;
    }

    Ok(())
}

/// Write a single plain-text file: title, author, description, then each chapter with a heading and stripped body.
pub fn write_text(book: &Book, path: &Path) -> Result<(), FormatError> {
    validate_book(book)?;

    let path = path.to_path_buf();
    let mut f = File::create(&path).map_err(|e| FormatError::Io {
        path: path.clone(),
        source: e,
    })?;

    writeln!(f, "{}", book.title)?;
    writeln!(f, "By {}", book.author)?;
    writeln!(f)?;
    if let Some(ref d) = book.description {
        writeln!(f, "{}", d)?;
        writeln!(f)?;
    }

    for ch in &book.chapters {
        writeln!(f)?;
        writeln!(f, "--- Chapter {}: {} ---", ch.index, ch.title)?;
        writeln!(f)?;
        let text = body_to_plain_text(&ch.body);
        writeln!(f, "{}", text)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Chapter;
    use std::io::Read;

    fn minimal_book() -> Book {
        Book {
            title: "Test Book".to_string(),
            author: "Test Author".to_string(),
            description: Some("A test.".to_string()),
            cover_url: None,
            chapters: vec![Chapter {
                title: "Chapter One".to_string(),
                index: 1,
                body: "<p>First paragraph.</p><p>Second paragraph.</p>".to_string(),
            }],
            source_url: None,
        }
    }

    #[test]
    fn write_html_contains_title_and_chapter_heading() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_test_html.html");
        write_html(&book, &path).unwrap();
        let mut buf = String::new();
        File::open(&path).unwrap().read_to_string(&mut buf).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(buf.contains("Test Book"));
        assert!(buf.contains("<h2>"));
        assert!(buf.contains("Chapter One"));
        assert!(buf.contains("First paragraph"));
    }

    #[test]
    fn write_markdown_contains_headers_and_no_raw_p_tags() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_test_md.md");
        write_markdown(&book, &path).unwrap();
        let mut buf = String::new();
        File::open(&path).unwrap().read_to_string(&mut buf).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(buf.starts_with("# Test Book"));
        assert!(buf.contains("## Chapter One"));
        assert!(buf.contains("First paragraph"));
        assert!(!buf.contains("<p>"));
    }

    #[test]
    fn write_text_contains_chapter_title_and_no_html_tags() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_test_txt.txt");
        write_text(&book, &path).unwrap();
        let mut buf = String::new();
        File::open(&path).unwrap().read_to_string(&mut buf).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(buf.contains("Test Book"));
        assert!(buf.contains("Chapter 1: Chapter One"));
        assert!(buf.contains("First paragraph"));
        assert!(!buf.contains("<p>"));
    }

    #[test]
    fn validate_rejects_empty_title() {
        let mut book = minimal_book();
        book.title.clear();
        let path = std::env::temp_dir().join("rdrscrape_void.html");
        assert!(matches!(
            write_html(&book, &path),
            Err(FormatError::EmptyTitle)
        ));
    }

    #[test]
    fn validate_rejects_empty_author() {
        let mut book = minimal_book();
        book.author.clear();
        let path = std::env::temp_dir().join("rdrscrape_void.html");
        assert!(matches!(
            write_html(&book, &path),
            Err(FormatError::EmptyAuthor)
        ));
    }

    #[test]
    fn body_to_plain_text_single_p() {
        assert_eq!(body_to_plain_text("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn body_to_plain_text_multiple_p() {
        let out = body_to_plain_text("<p>A</p><p>B</p>");
        assert!(out.contains("A"));
        assert!(out.contains("B"));
    }

    #[test]
    fn body_to_plain_text_plain_text_fallback() {
        let raw = "No tags here.";
        assert_eq!(body_to_plain_text(raw), "No tags here.");
    }

    #[test]
    fn body_to_plain_text_whitespace_only_fallback() {
        let out = body_to_plain_text("   \n  ");
        assert_eq!(out, "");
    }

    #[test]
    fn html_escape_attr_escapes_special_chars() {
        assert_eq!(html_escape_attr("a & b"), "a &amp; b");
        assert_eq!(html_escape_attr("<tag>"), "&lt;tag&gt;");
        assert_eq!(html_escape_attr(r#"say "hi""#), "say &quot;hi&quot;");
    }

    #[test]
    fn html_escape_attr_no_double_escape() {
        let once = html_escape_attr("a & b");
        let twice = html_escape_attr(&once);
        assert_eq!(once, "a &amp; b");
        assert_eq!(twice, "a &amp;amp; b");
    }
}
