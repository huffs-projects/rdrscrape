//! Royal Road adapter. Fetches fiction page (metadata + TOC) then each chapter; produces canonical Book.
//!
//! Cloudflare: cookie jar and browser-like User-Agent are used; captcha is not handled (see README.md, Known edge cases).

use crate::model::{Book, Chapter};
use crate::scraper::error::ScraperError;
use crate::scraper::{
    strip_title_site_suffix, EmptyChapterBehavior, LockedChapterBehavior, PoliteClient,
    ScrapeOptions, Scraper,
};
use reqwest::Url;
use scraper::{Html, Selector};
use serde::Deserialize;

const ROYALROAD_BASE: &str = "https://www.royalroad.com";

/// Parse a CSS selector or return a parse error (avoids panics from Selector::parse).
fn parse_selector(sel: &str) -> Result<Selector, ScraperError> {
    Selector::parse(sel).map_err(|e| ScraperError::ParseStoryPage {
        message: format!("invalid selector {:?}: {}", sel, e),
    })
}

/// Royal Road scraper. Holds a reference to the shared polite client.
pub struct RoyalRoadScraper<'a> {
    client: &'a mut PoliteClient,
}

/// Shape of one entry in window.chapters (relative url, order 0-based, isUnlocked).
#[derive(Debug, Deserialize)]
struct WindowChapter {
    #[allow(dead_code)]
    id: u64,
    title: String,
    url: String,
    #[serde(default)]
    order: u32,
    #[serde(rename = "isUnlocked", default = "default_true")]
    is_unlocked: bool,
}

fn default_true() -> bool {
    true
}

/// Require fiction URL (no /chapter/ in path). Returns the URL as-is if valid.
fn ensure_fiction_url(url: &str) -> Result<String, ScraperError> {
    let parsed = Url::parse(url).map_err(|e| ScraperError::InvalidUrl {
        input: url.to_string(),
        reason: e.to_string(),
    })?;
    let path = parsed.path();
    if path.contains("/chapter/") {
        return Err(ScraperError::ParseStoryPage {
            message: "Expected a fiction (index) URL, not a chapter URL. Use the story page, e.g. https://www.royalroad.com/fiction/21220/mother-of-learning".to_string(),
        });
    }
    Ok(url.to_string())
}

/// Check response status and read body as UTF-8. Returns body or ScraperError.
fn check_response(
    response: reqwest::blocking::Response,
    url: &str,
    context: Option<&str>,
) -> Result<String, ScraperError> {
    let status = response.status();
    if !status.is_success() {
        return Err(ScraperError::HttpStatus {
            status: status.as_u16(),
            url: url.to_string(),
            context: context.map(String::from),
        });
    }
    response
        .text()
        .map_err(|e| ScraperError::BodyRead { source: e })
}

/// Extract metadata from fiction page HTML: JSON-LD Book first, then DOM fallback.
fn parse_metadata(
    html: &str,
) -> Result<(String, String, Option<String>, Option<String>), ScraperError> {
    // Prefer JSON-LD @type "Book"
    if let Some(script) = html.find("<script type=\"application/ld+json\">") {
        let start = script + "<script type=\"application/ld+json\">".len();
        let end = html[start..]
            .find("</script>")
            .map(|i| start + i)
            .unwrap_or(html.len());
        let json_str = html[start..end].trim();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
            if v.get("@type").and_then(|t| t.as_str()) == Some("Book") {
                let title = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(String::from)
                    .filter(|s| !s.is_empty());
                let author = v
                    .get("author")
                    .and_then(|a| a.get("name"))
                    .and_then(|n| n.as_str())
                    .map(String::from)
                    .filter(|s| !s.is_empty());
                let description = v
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(strip_html_tags)
                    .filter(|s| !s.is_empty());
                let cover_url = v
                    .get("image")
                    .and_then(|i| i.as_str())
                    .map(String::from)
                    .filter(|s| !s.is_empty());
                if let (Some(t), Some(a)) = (title, author) {
                    return Ok((t, a, description, cover_url));
                }
            }
        }
    }

    // Fallback: DOM selectors
    let doc = Html::parse_document(html);
    let title_sel = parse_selector("h1.font-white")?;
    let author_sel = parse_selector("h4 a.font-white")?;
    let desc_sel = parse_selector(".description")?;
    let cover_sel = parse_selector("meta[property=\"og:image\"]")?;
    let title = doc
        .select(&title_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty());
    let author = doc
        .select(&author_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty());
    let description = doc
        .select(&desc_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty());
    let cover_url = doc
        .select(&cover_sel)
        .next()
        .and_then(|e| e.value().attr("content").map(String::from))
        .filter(|s| !s.is_empty());

    match (title, author) {
        (Some(t), Some(a)) => Ok((t, a, description, cover_url)),
        _ => Err(ScraperError::ParseStoryPage {
            message: "missing title or author (selector or structure may have changed)".to_string(),
        }),
    }
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("\n\n\n", "\n\n").trim().to_string()
}

/// Extract window.chapters array from script. Returns (index_1based, full_url, title, is_unlocked).
/// Relative URLs resolved against ROYALROAD_BASE.
fn parse_toc_with_locked(html: &str) -> Result<Vec<(u32, String, String, bool)>, ScraperError> {
    let needle = "window.chapters = ";
    let start = html
        .find(needle)
        .ok_or_else(|| ScraperError::ChapterListParse {
            reason: "window.chapters not found".to_string(),
        })?;
    let after_assign = start + needle.len();
    let bracket = html[after_assign..]
        .find('[')
        .ok_or_else(|| ScraperError::ChapterListParse {
            reason: "window.chapters array start not found".to_string(),
        })?;
    let array_start = after_assign + bracket;
    let array_slice = extract_json_array_with_strings(&html[array_start..]).ok_or_else(|| {
        ScraperError::ChapterListParse {
            reason: "could not extract window.chapters array".to_string(),
        }
    })?;
    let chapters: Vec<WindowChapter> =
        serde_json::from_str(array_slice).map_err(|e| ScraperError::ChapterListParse {
            reason: e.to_string(),
        })?;
    let base = Url::parse(ROYALROAD_BASE).map_err(|e| ScraperError::ChapterListParse {
        reason: e.to_string(),
    })?;
    let mut toc = Vec::with_capacity(chapters.len());
    for ch in chapters {
        let full_url = base
            .join(ch.url.trim_start_matches('/'))
            .map_err(|e| ScraperError::ChapterListParse {
                reason: e.to_string(),
            })?
            .to_string();
        let index = ch.order + 1;
        toc.push((index, full_url, ch.title, ch.is_unlocked));
    }
    toc.sort_by_key(|(i, _, _, _)| *i);
    Ok(toc)
}

/// Like parse_toc_with_locked but only returns unlocked chapters (used by tests).
#[allow(dead_code)]
fn parse_toc(html: &str) -> Result<Vec<(u32, String, String)>, ScraperError> {
    let toc = parse_toc_with_locked(html)?;
    let unlocked: Vec<_> = toc
        .into_iter()
        .filter(|(_, _, _, u)| *u)
        .map(|(i, url, title, _)| (i, url, title))
        .collect();
    if unlocked.is_empty() {
        return Err(ScraperError::EmptyChapterList);
    }
    Ok(unlocked)
}

/// Find the matching closing bracket for the first '[' in s, skipping content inside JSON strings.
fn extract_json_array_with_strings(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (byte_offset, c) in s[start..].char_indices() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' {
                escape = true;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + byte_offset + 1]);
                }
            }
            '"' => in_string = true,
            _ => {}
        }
    }
    None
}

/// Parse chapter page HTML for title and body. Body is direct child <p> of div.chapter-inner.chapter-content.
fn parse_chapter_page(html: &str, index: u32, url: &str) -> Result<(String, String), ScraperError> {
    let doc = Html::parse_document(html);

    let h1_sel = parse_selector("h1.font-white.break-word")?;
    let og_title_sel = parse_selector("meta[property=\"og:title\"]")?;
    let title_sel = parse_selector("title")?;
    let title = doc
        .select(&h1_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.select(&og_title_sel)
                .next()
                .and_then(|e| e.value().attr("content"))
                .map(|s| {
                    strip_title_site_suffix(
                        s.trim(),
                        &[" _ Royal Road", " - Royal Road", " | Royal Road"],
                    )
                })
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            doc.select(&title_sel)
                .next()
                .and_then(|e| e.text().next())
                .map(|t| {
                    strip_title_site_suffix(
                        t.trim(),
                        &[" _ Royal Road", " - Royal Road", " | Royal Road"],
                    )
                })
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| format!("Chapter {}", index));

    let container_sel = parse_selector("div.chapter-inner.chapter-content")?;
    let has_container = doc.select(&container_sel).next().is_some();
    if !has_container {
        return Err(ScraperError::ParseChapter {
            index,
            url: url.to_string(),
        });
    }

    // Direct child <p> only; ignore obfuscated classes. Output minimal HTML <p>...</p>.
    let p_sel = parse_selector("div.chapter-inner.chapter-content > p")?;
    let body = doc
        .select(&p_sel)
        .map(|el| {
            let text = el.text().collect::<String>().trim().to_string();
            format!("<p>{}</p>", html_escape_inner(&text))
        })
        .collect::<Vec<_>>()
        .join("");
    if body.is_empty() {
        return Err(ScraperError::ParseChapter {
            index,
            url: url.to_string(),
        });
    }

    Ok((title, body))
}

fn html_escape_inner(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl<'a> RoyalRoadScraper<'a> {
    pub fn new(client: &'a mut PoliteClient) -> Self {
        Self { client }
    }
}

impl Scraper for RoyalRoadScraper<'_> {
    fn scrape_book(
        &mut self,
        url: &str,
        options: &ScrapeOptions<'_>,
    ) -> Result<Book, ScraperError> {
        let fiction_url = ensure_fiction_url(url)?;

        let response =
            self.client
                .get_with_retry(&fiction_url)
                .map_err(|e| ScraperError::Network {
                    url: fiction_url.clone(),
                    source: e,
                })?;
        let html = check_response(response, &fiction_url, Some("story page"))?;

        let mut toc = parse_toc_with_locked(&html)?;
        let locked_count = toc.iter().filter(|(_, _, _, u)| !*u).count();
        if locked_count > 0
            && options
                .locked_behavior
                .unwrap_or(LockedChapterBehavior::Skip)
                == LockedChapterBehavior::Fail
        {
            return Err(ScraperError::LockedChaptersNotAllowed {
                count: locked_count,
            });
        }

        let total = toc.len() as u32;
        if let Some((from, to)) = options.chapter_range {
            toc.retain(|(index, _, _, _)| *index >= from && *index <= to);
        }

        let mut book: Book = if let Some(init) = options.initial_book {
            init.clone()
        } else {
            let (title, author, description, cover_url) = parse_metadata(&html)?;
            Book {
                title,
                author,
                description,
                cover_url,
                chapters: Vec::with_capacity(toc.len()),
                source_url: Some(fiction_url),
            }
        };

        if options.toc_only {
            let lb = options
                .locked_behavior
                .unwrap_or(LockedChapterBehavior::Skip);
            for (index, _chapter_url, title, is_unlocked) in toc {
                if book.chapters.iter().any(|c| c.index == index) {
                    continue;
                }
                if !is_unlocked {
                    match lb {
                        LockedChapterBehavior::Skip => continue,
                        LockedChapterBehavior::Placeholder => {
                            book.chapters.push(Chapter {
                                title: format!("{} (locked)", title),
                                index,
                                body: String::new(),
                            });
                        }
                        LockedChapterBehavior::Fail => {}
                    }
                } else {
                    book.chapters.push(Chapter {
                        title,
                        index,
                        body: String::new(),
                    });
                }
            }
            book.chapters.sort_by_key(|c| c.index);
            return Ok(book);
        }

        let mut done = 0u32;
        for (index, chapter_url, title, is_unlocked) in toc {
            if book.chapters.iter().any(|c| c.index == index) {
                continue;
            }
            done += 1;
            if let Some(ref p) = options.progress {
                p(done, total);
            }

            if !is_unlocked {
                match options
                    .locked_behavior
                    .unwrap_or(LockedChapterBehavior::Skip)
                {
                    LockedChapterBehavior::Skip => continue,
                    LockedChapterBehavior::Placeholder => {
                        let placeholder_title = format!("{} (locked)", title);
                        let placeholder_body =
                            "<p>This chapter is locked (premium) and could not be retrieved.</p>"
                                .to_string();
                        book.chapters.push(Chapter {
                            title: placeholder_title,
                            index,
                            body: placeholder_body,
                        });
                        book.chapters.sort_by_key(|c| c.index);
                        if let Some(ref cb) = options.on_checkpoint {
                            cb(&book);
                        }
                        continue;
                    }
                    LockedChapterBehavior::Fail => {
                        return Err(ScraperError::LockedChaptersNotAllowed {
                            count: locked_count,
                        });
                    }
                }
            }

            let response = match self.client.get_with_retry(&chapter_url) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "Chapter {}: network error at {}: {}. Skipped.",
                        index, chapter_url, e
                    );
                    continue;
                }
            };

            if !response.status().is_success() {
                eprintln!(
                    "Chapter {}: HTTP {} at {}. Skipped.",
                    index,
                    response.status().as_u16(),
                    chapter_url
                );
                continue;
            }

            let chapter_html = match response.text() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Chapter {}: failed to read body: {}. Skipped.", index, e);
                    continue;
                }
            };

            let empty_behavior = options
                .empty_chapter_behavior
                .unwrap_or(EmptyChapterBehavior::Skip);
            match parse_chapter_page(&chapter_html, index, &chapter_url) {
                Ok((parsed_title, body)) => {
                    if body.is_empty() {
                        match empty_behavior {
                            EmptyChapterBehavior::Skip => {
                                eprintln!(
                                    "Chapter {} returned no content at {}. Skipped.",
                                    index, chapter_url
                                );
                                continue;
                            }
                            EmptyChapterBehavior::Placeholder => {
                                book.chapters.push(Chapter {
                                    title: format!("{} (no content)", parsed_title),
                                    index,
                                    body: "<p>This chapter returned no content.</p>".to_string(),
                                });
                                book.chapters.sort_by_key(|c| c.index);
                                if let Some(ref cb) = options.on_checkpoint {
                                    cb(&book);
                                }
                            }
                            EmptyChapterBehavior::Fail => {
                                return Err(ScraperError::EmptyChapter {
                                    index,
                                    url: chapter_url.clone(),
                                });
                            }
                        }
                        continue;
                    }
                    book.chapters.push(Chapter {
                        title: parsed_title,
                        index,
                        body,
                    });
                    book.chapters.sort_by_key(|c| c.index);
                    if let Some(ref cb) = options.on_checkpoint {
                        cb(&book);
                    }
                }
                Err(ScraperError::ParseChapter { index: pi, url: u }) => match empty_behavior {
                    EmptyChapterBehavior::Skip => {
                        eprintln!("Chapter {}: could not parse content at {}. Skipped.", pi, u);
                    }
                    EmptyChapterBehavior::Placeholder => {
                        book.chapters.push(Chapter {
                                title: format!("Chapter {} (unable to parse)", pi),
                                index: pi,
                                body: "<p>This chapter could not be parsed (missing content container).</p>"
                                    .to_string(),
                            });
                        book.chapters.sort_by_key(|c| c.index);
                        if let Some(ref cb) = options.on_checkpoint {
                            cb(&book);
                        }
                    }
                    EmptyChapterBehavior::Fail => {
                        return Err(ScraperError::ParseChapter { index: pi, url: u });
                    }
                },
                Err(e) => return Err(e),
            }
        }

        if book.chapters.is_empty() {
            return Err(ScraperError::NoChaptersRetrieved);
        }

        Ok(book)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn inline_parse_metadata_json_ld() -> Result<(), ScraperError> {
        let html = r#"<html><head></head><body>
<script type="application/ld+json">
{"@type":"Book","name":"Inline Test Book","author":{"name":"Inline Author"},"description":"A description.","image":"https://example.com/cover.png"}
</script>
</body></html>"#;
        let (title, author, description, cover_url) = parse_metadata(html)?;
        assert_eq!(title, "Inline Test Book");
        assert_eq!(author, "Inline Author");
        assert_eq!(description.as_deref(), Some("A description."));
        assert_eq!(cover_url.as_deref(), Some("https://example.com/cover.png"));
        Ok(())
    }

    #[test]
    fn inline_parse_toc() -> Result<(), ScraperError> {
        let html = r#"<script>
window.chapters = [{"id":101,"title":"Ch 1","url":"/fiction/1/slug/chapter/1/ch-1","order":0,"isUnlocked":true}];
</script>"#;
        let toc = parse_toc(html)?;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].0, 1);
        assert!(toc[0].1.contains("royalroad.com"));
        assert!(toc[0].1.ends_with("/ch-1"));
        assert_eq!(toc[0].2, "Ch 1");
        Ok(())
    }

    #[test]
    fn inline_parse_toc_skips_locked() -> Result<(), ScraperError> {
        let html = r#"<script>
window.chapters = [{"id":1,"title":"Free","url":"/fiction/1/s/free","order":0,"isUnlocked":true},{"id":2,"title":"Locked","url":"/fiction/1/s/locked","order":1,"isUnlocked":false}];
</script>"#;
        let toc = parse_toc(html)?;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].2, "Free");
        Ok(())
    }

    #[test]
    fn inline_parse_chapter_page() -> Result<(), ScraperError> {
        let html = r#"<!DOCTYPE html><html><head><meta property="og:title" content="1. Good Morning - Book _ Royal Road"/></head><body>
<h1 class="font-white break-word">1. Good Morning</h1>
<div class="chapter-inner chapter-content">
<p>First paragraph here.</p>
<p>Second paragraph.</p>
</div>
</body></html>"#;
        let (title, body) = parse_chapter_page(
            html,
            1,
            "https://www.royalroad.com/fiction/1/slug/chapter/1/good-morning",
        )?;
        assert_eq!(title, "1. Good Morning");
        assert!(body.contains("<p>"));
        assert!(body.contains("First paragraph here"));
        assert!(body.contains("Second paragraph"));
        Ok(())
    }

    #[test]
    fn inline_parse_chapter_page_title_fallback_with_dash_and_pipe() -> Result<(), ScraperError> {
        // No h1; title from og:title. Chapter title contains " - " and suffix uses " _ ".
        let html = r#"<!DOCTYPE html><html><head><meta property="og:title" content="1. Good Morning - Brother - Book _ Royal Road"/></head><body>
<div class="chapter-inner chapter-content"><p>Content.</p></div></body></html>"#;
        let (title, _) =
            parse_chapter_page(html, 1, "https://www.royalroad.com/fiction/1/s/chapter/1")?;
        assert_eq!(title, "1. Good Morning - Brother - Book");
        Ok(())
    }

    /// Fixture test: parse fiction page and chapter page from saved HTML fixtures.
    /// Skips if fixture files are not present (e.g. in CI). Returns Err to fail test without panicking.
    #[test]
    fn fixture_fiction_and_chapter_parse() -> Result<(), ScraperError> {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };
        let base = Path::new(&manifest_dir).join("Royal Road");
        let fiction_path = base.join("Mother of Learning _ Royal Road.html");
        let chapter_path =
            base.join("1. Good Morning Brother - Mother of Learning _ Royal Road.html");

        let fiction_html = match std::fs::read_to_string(&fiction_path) {
            Ok(s) => s,
            Err(_) => return Ok(()), // skip if fixtures not present
        };

        let (title, author, description, cover_url) = parse_metadata(&fiction_html)?;
        assert_eq!(title, "Mother of Learning");
        assert_eq!(author, "nobody103");
        assert!(description.is_some());
        assert!(cover_url.is_some());

        let toc = parse_toc(&fiction_html)?;
        assert!(!toc.is_empty());
        assert_eq!(toc[0].0, 1);
        assert!(toc[0].1.contains("royalroad.com"));
        assert_eq!(toc[0].2, "1. Good Morning Brother");

        let chapter_html = match std::fs::read_to_string(&chapter_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };
        let (ch_title, body) = parse_chapter_page(&chapter_html, 1, "https://www.royalroad.com/fiction/21220/mother-of-learning/chapter/301778/1-good-morning-brother")?;
        assert_eq!(ch_title, "1. Good Morning Brother");
        assert!(!body.is_empty());
        assert!(body.contains("<p>"));
        Ok(())
    }

    /// Fixture test: parse fiction and chapter from "Imma be a speedster" saved HTML.
    /// Skips if fixture files are not present.
    #[test]
    fn fixture_imma_be_a_speedster_parse() -> Result<(), ScraperError> {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };
        let base = Path::new(&manifest_dir).join("Royal Road");
        let fiction_path = base.join("Imma be a speedster _ Royal Road.html");
        let chapter_path =
            base.join("Chapter 1 - Smart decisions - Imma be a speedster _ Royal Road.html");

        let fiction_html = match std::fs::read_to_string(&fiction_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };

        let (title, author, description, cover_url) = parse_metadata(&fiction_html)?;
        assert_eq!(title, "Imma be a speedster");
        assert_eq!(author, "UnproperMadman");
        assert!(description.is_some());
        assert!(cover_url.is_some());

        let toc = parse_toc(&fiction_html)?;
        assert!(!toc.is_empty());
        assert_eq!(toc[0].0, 1);
        assert!(toc[0].1.contains("royalroad.com"));
        assert_eq!(toc[0].2, "Chapter 1 - Smart decisions");

        let chapter_html = match std::fs::read_to_string(&chapter_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };
        let (ch_title, body) = parse_chapter_page(
            &chapter_html,
            1,
            "https://www.royalroad.com/fiction/136335/imma-be-a-speedster/chapter/123/chapter-1-smart-decisions",
        )?;
        assert_eq!(ch_title, "Chapter 1 - Smart decisions");
        assert!(!body.is_empty());
        assert!(body.contains("<p>"));
        Ok(())
    }
}
