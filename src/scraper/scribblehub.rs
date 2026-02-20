//! Scribble Hub adapter. Fetches series page (metadata + TOC, with pagination) then each chapter; produces canonical Book.
//!
//! TOC source: series page only. Chapter body: #chp_raw only (see README.md, Known edge cases).

use crate::model::{Book, Chapter};
use crate::scraper::error::ScraperError;
use crate::scraper::{
    strip_title_site_suffix, EmptyChapterBehavior, PoliteClient, ScrapeOptions, Scraper,
};
use reqwest::Url;
use scraper::{Html, Selector};

const SCRIBBLEHUB_BASE: &str = "https://www.scribblehub.com";

/// Parse a CSS selector or return a parse error (avoids panics from Selector::parse).
fn parse_selector(sel: &str) -> Result<Selector, ScraperError> {
    Selector::parse(sel).map_err(|e| ScraperError::ParseStoryPage {
        message: format!("invalid selector {:?}: {}", sel, e),
    })
}

/// Scribble Hub scraper. Holds a reference to the shared polite client.
pub struct ScribbleHubScraper<'a> {
    client: &'a mut PoliteClient,
}

/// Extract series ID from URL path /series/{id}/{slug}/. Returns None if not found.
fn extract_series_id_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let path = parsed.path();
    let after_series = path.strip_prefix("/series/")?;
    let id = after_series.split('/').next()?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(id.to_string())
}

/// Require series URL (path contains /series/; reject /read/.../chapter/). Returns the URL as-is if valid.
fn ensure_series_url(url: &str) -> Result<String, ScraperError> {
    let parsed = Url::parse(url).map_err(|e| ScraperError::InvalidUrl {
        input: url.to_string(),
        reason: e.to_string(),
    })?;
    let host = parsed.host_str().ok_or_else(|| ScraperError::InvalidUrl {
        input: url.to_string(),
        reason: "URL has no host".to_string(),
    })?;
    if !host.contains("scribblehub.com") {
        return Err(ScraperError::ParseStoryPage {
            message: "Expected a Scribble Hub series URL (host scribblehub.com).".to_string(),
        });
    }
    let path = parsed.path();
    if path.contains("/read/") && path.contains("/chapter/") {
        return Err(ScraperError::ParseStoryPage {
            message: "Expected a series (index) URL, not a chapter URL. Use the series page, e.g. https://www.scribblehub.com/series/862913/hp-the-arcane-thief-litrpg/".to_string(),
        });
    }
    if !path.contains("/series/") {
        return Err(ScraperError::ParseStoryPage {
            message: "Expected a series URL containing /series/{id}/{slug}/.".to_string(),
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

const LD_JSON_OPEN: &str = "<script type=\"application/ld+json\">";
const LD_JSON_CLOSE: &str = "</script>";

/// Extract metadata from series page HTML: JSON-LD Book first (scan all ld+json scripts for @type Book), then DOM fallback.
fn parse_metadata(
    html: &str,
) -> Result<(String, String, Option<String>, Option<String>), ScraperError> {
    let mut search_start = 0;
    while let Some(script) = html[search_start..].find(LD_JSON_OPEN) {
        let start = search_start + script + LD_JSON_OPEN.len();
        let end = html[start..]
            .find(LD_JSON_CLOSE)
            .map(|i| start + i)
            .unwrap_or(html.len());
        let json_str = html[start..end].trim();
        search_start = end + LD_JSON_CLOSE.len();

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

    let doc = Html::parse_document(html);
    let fic_title_sel = parse_selector("div.fic_title")?;
    let og_title_sel = parse_selector("meta[property=\"og:title\"]")?;
    let author_span_sel =
        parse_selector("div.sb_content.author div[property=\"author\"] a span.auth_name_fic")?;
    let author_a_sel = parse_selector("div.sb_content.author div[property=\"author\"] a")?;
    let og_image_sel = parse_selector("meta[property=\"og:image\"]")?;
    let title = doc
        .select(&fic_title_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.select(&og_title_sel)
                .next()
                .and_then(|e| e.value().attr("content").map(String::from))
                .filter(|s| !s.is_empty())
        });
    let author = doc
        .select(&author_span_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.select(&author_a_sel)
                .next()
                .map(|e| e.text().collect::<String>().trim().to_string())
                .filter(|s| !s.is_empty())
        });
    let cover_url = doc
        .select(&og_image_sel)
        .next()
        .and_then(|e| e.value().attr("content").map(String::from))
        .filter(|s| !s.is_empty());

    match (title, author) {
        (Some(t), Some(a)) => Ok((t, a, None, cover_url)),
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

/// Parse one page's TOC: ol.toc_ol > li.toc_w (order attr), a.toc_a (href, text). Returns (order, full_url, title).
fn parse_toc_page(html: &str, base: &Url) -> Result<Vec<(u32, String, String)>, ScraperError> {
    let doc = Html::parse_document(html);
    let ol_sel = parse_selector("ol.toc_ol")?;
    let li_sel = parse_selector("li.toc_w")?;
    let a_sel = parse_selector("a.toc_a")?;

    let ol = doc
        .select(&ol_sel)
        .next()
        .ok_or_else(|| ScraperError::ChapterListParse {
            reason: "ol.toc_ol not found".to_string(),
        })?;

    let mut entries = Vec::new();
    for li in ol.select(&li_sel) {
        let order = li
            .value()
            .attr("order")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let a = match li.select(&a_sel).next() {
            Some(link) => link,
            None => continue,
        };
        let href = match a.value().attr("href") {
            Some(h) => h,
            None => continue,
        };
        let full_url = base
            .join(href.trim_start_matches('/'))
            .map_err(|e| ScraperError::ChapterListParse {
                reason: e.to_string(),
            })?
            .to_string();
        let title = a.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }
        entries.push((order, full_url, title));
    }
    Ok(entries)
}

/// Parse toc=N from a URL or query string. Returns 1 if missing.
fn parse_toc_page_from_url(url: &str) -> u32 {
    let query = url.split('?').nth(1).unwrap_or("");
    for param in query.split('&') {
        let param = param.trim();
        if let Some(rest) = param.strip_prefix("toc=") {
            let n = rest.split(['#', '&']).next().unwrap_or("").trim();
            if let Ok(num) = n.parse::<u32>() {
                return num;
            }
        }
    }
    1
}

/// Find next TOC page URL from #pagination-mesh-toc a.page-link.next, or fallback: any a in
/// #pagination-mesh-toc with href containing toc=(current+1). Scribble Hub sometimes omits the
/// .next class on the "»" link. Returns None if no next page.
fn next_toc_page_url(html: &str, series_base: &Url, current_page_url: Option<&str>) -> Option<String> {
    let doc = Html::parse_document(html);
    let current_page = current_page_url.map(parse_toc_page_from_url).unwrap_or(1);

    let next_sel = parse_selector("#pagination-mesh-toc a.page-link.next").ok()?;
    if let Some(next_a) = doc.select(&next_sel).next() {
        let href = next_a.value().attr("href")?;
        if !href.is_empty() && href != "#" {
            if let Ok(u) = series_base.join(href) {
                return Some(u.to_string());
            }
        }
    }

    let next_page = current_page + 1;
    for sel in [
        "#pagination-mesh-toc a.page-link[href*=\"toc=\"]",
        "#pagination-mesh-toc a[href*=\"toc=\"]",
    ] {
        let fallback_sel = match parse_selector(sel) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for a in doc.select(&fallback_sel) {
            let href = match a.value().attr("href") {
                Some(h) if !h.is_empty() && h != "#" => h,
                _ => continue,
            };
            let n = parse_toc_page_from_url(href);
            if n == next_page {
                if let Ok(u) = series_base.join(href) {
                    return Some(u.to_string());
                }
            }
        }
    }
    None
}

/// Sort TOC entries by order and deduplicate by URL (first occurrence kept). Used when merging multiple TOC pages.
fn merge_toc_entries(mut all_entries: Vec<(u32, String, String)>) -> Vec<(u32, String, String)> {
    all_entries.sort_by_key(|(order, _, _)| *order);
    let mut seen = std::collections::HashSet::new();
    all_entries.retain(|(_, url, _)| seen.insert(url.clone()));
    all_entries
}

const SCRIBBLEHUB_AJAX_URL: &str = "https://www.scribblehub.com/wp-admin/admin-ajax.php";

/// Fetch full TOC via ScribbleHub's AJAX "Show All Chapters" (wi_getreleases_pagination pagenum=-1).
/// Returns all chapters in one request. ScribbleHub loads the TOC via JavaScript; the initial HTML
/// only contains ~15 chapters per page, so pagination often fails. The AJAX endpoint returns all.
fn fetch_full_toc_via_ajax(
    client: &mut PoliteClient,
    series_url: &str,
) -> Option<Result<Vec<(u32, String, String)>, ScraperError>> {
    let mypostid = extract_series_id_from_url(series_url)?;
    let base = match Url::parse(SCRIBBLEHUB_BASE) {
        Ok(b) => b,
        Err(e) => return Some(Err(ScraperError::ChapterListParse {
            reason: e.to_string(),
        })),
    };

    let response = match client.post_form(
        SCRIBBLEHUB_AJAX_URL,
        &[
            ("action", "wi_getreleases_pagination"),
            ("pagenum", "-1"),
            ("mypostid", &mypostid),
        ],
    ) {
        Ok(r) => r,
        Err(e) => {
            return Some(Err(ScraperError::Network {
                url: SCRIBBLEHUB_AJAX_URL.to_string(),
                source: e,
            }))
        }
    };
    let html = match check_response(response, SCRIBBLEHUB_AJAX_URL, Some("TOC AJAX")) {
        Ok(h) => h,
        Err(e) => return Some(Err(e)),
    };
    Some(parse_toc_page(&html, &base).map(merge_toc_entries))
}

/// Fetch full TOC: try AJAX "Show All" first (reliable), then fall back to paginated requests.
/// Returns (order, full_url, title) sorted by reading order, deduplicated by URL.
fn fetch_full_toc(
    client: &mut PoliteClient,
    series_url: &str,
    first_page_html: &str,
) -> Result<Vec<(u32, String, String)>, ScraperError> {
    if let Some(result) = fetch_full_toc_via_ajax(client, series_url) {
        let entries = result?;
        if !entries.is_empty() {
            return Ok(entries);
        }
    }

    let base = Url::parse(SCRIBBLEHUB_BASE).map_err(|e| ScraperError::ChapterListParse {
        reason: e.to_string(),
    })?;
    let series_base = Url::parse(series_url).map_err(|e| ScraperError::ChapterListParse {
        reason: e.to_string(),
    })?;

    let mut all_entries = parse_toc_page(first_page_html, &base)?;
    let mut current_url = next_toc_page_url(first_page_html, &series_base, Some(series_url));

    while let Some(next_url) = current_url.clone() {
        let response = client
            .get_with_retry(&next_url)
            .map_err(|e| ScraperError::Network {
                url: next_url.clone(),
                source: e,
            })?;
        let html = check_response(response, &next_url, Some("TOC page"))?;
        let page_entries = parse_toc_page(&html, &base)?;
        all_entries.extend(page_entries);
        current_url = next_toc_page_url(&html, &series_base, Some(&next_url));
    }

    let all_entries = merge_toc_entries(all_entries);
    if all_entries.is_empty() {
        return Err(ScraperError::EmptyChapterList);
    }
    Ok(all_entries)
}

/// Parse chapter page: title from div.chapter-title or <title>; body from #chp_raw.chp_raw direct children.
fn parse_chapter_page(html: &str, index: u32, url: &str) -> Result<(String, String), ScraperError> {
    let doc = Html::parse_document(html);

    let chapter_title_sel = parse_selector("div.chapter-title")?;
    let title_sel = parse_selector("title")?;
    let title = doc
        .select(&chapter_title_sel)
        .next()
        .map(|e| e.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            doc.select(&title_sel)
                .next()
                .and_then(|e| e.text().next())
                .map(|t| strip_title_site_suffix(t.trim(), &[" | Scribble Hub", " - Scribble Hub"]))
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| format!("Chapter {}", index));

    let chp_raw_sel = parse_selector("#chp_raw.chp_raw")?;
    if doc.select(&chp_raw_sel).next().is_none() {
        return Err(ScraperError::ParseChapter {
            index,
            url: url.to_string(),
        });
    }

    let p_sel = parse_selector("#chp_raw.chp_raw > p")?;
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

impl<'a> ScribbleHubScraper<'a> {
    pub fn new(client: &'a mut PoliteClient) -> Self {
        Self { client }
    }
}

impl Scraper for ScribbleHubScraper<'_> {
    fn scrape_book(
        &mut self,
        url: &str,
        options: &ScrapeOptions<'_>,
    ) -> Result<Book, ScraperError> {
        let series_url = ensure_series_url(url)?;

        let response =
            self.client
                .get_with_retry(&series_url)
                .map_err(|e| ScraperError::Network {
                    url: series_url.clone(),
                    source: e,
                })?;
        let html = check_response(response, &series_url, Some("story page"))?;

        let mut toc = fetch_full_toc(self.client, &series_url, &html)?;
        let total = toc.len() as u32;
        if let Some((from, to)) = options.chapter_range {
            toc.retain(|(index, _, _)| *index >= from && *index <= to);
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
                source_url: Some(series_url),
            }
        };

        if options.toc_only {
            for (index, _chapter_url, title) in toc {
                if book.chapters.iter().any(|c| c.index == index) {
                    continue;
                }
                book.chapters.push(Chapter {
                    title,
                    index,
                    body: String::new(),
                });
            }
            book.chapters.sort_by_key(|c| c.index);
            return Ok(book);
        }

        let mut done = 0u32;
        for (index, chapter_url, _) in toc {
            if book.chapters.iter().any(|c| c.index == index) {
                continue;
            }
            if options.cancel_check.map(|c| c()).unwrap_or(false) {
                return Err(ScraperError::Cancelled);
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
                                done += 1;
                                if let Some(ref p) = options.progress {
                                    p(done, total);
                                }
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
                    done += 1;
                    if let Some(ref p) = options.progress {
                        p(done, total);
                    }
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
                        done += 1;
                        if let Some(ref p) = options.progress {
                            p(done, total);
                        }
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
{"@type":"Book","name":"SH Inline Book","author":{"name":"SH Author"},"description":"Desc","image":"https://example.com/cover.jpg"}
</script>
</body></html>"#;
        let (title, author, description, cover_url) = parse_metadata(html)?;
        assert_eq!(title, "SH Inline Book");
        assert_eq!(author, "SH Author");
        assert_eq!(description.as_deref(), Some("Desc"));
        assert_eq!(cover_url.as_deref(), Some("https://example.com/cover.jpg"));
        Ok(())
    }

    #[test]
    fn inline_parse_toc_page() -> Result<(), ScraperError> {
        let base_url =
            Url::parse(SCRIBBLEHUB_BASE).map_err(|e| ScraperError::ChapterListParse {
                reason: e.to_string(),
            })?;
        let html = r#"<html><body>
<ol class="toc_ol">
<li class="toc_w" order="1"><a class="toc_a" href="/read/123/series-slug/chapter/1/">Chapter 1: Start</a></li>
</ol>
</body></html>"#;
        let entries = parse_toc_page(html, &base_url)?;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].1.contains("scribblehub.com"));
        assert_eq!(entries[0].2, "Chapter 1: Start");
        Ok(())
    }

    #[test]
    fn inline_parse_chapter_page() -> Result<(), ScraperError> {
        let html = r#"<!DOCTYPE html><html><head><title>Book - Chapter 1: Intro | Scribble Hub</title></head><body>
<div class="chapter-title">Chapter 1: Intro</div>
<div id="chp_raw" class="chp_raw">
<p>First line of the chapter.</p>
<p>Second line.</p>
</div>
</body></html>"#;
        let (title, body) = parse_chapter_page(
            html,
            1,
            "https://www.scribblehub.com/read/123/slug/chapter/1/",
        )?;
        assert_eq!(title, "Chapter 1: Intro");
        assert!(body.contains("<p>"));
        assert!(body.contains("First line of the chapter"));
        assert!(body.contains("Second line"));
        Ok(())
    }

    #[test]
    fn inline_parse_chapter_page_title_fallback_with_dash_and_pipe() -> Result<(), ScraperError> {
        // No div.chapter-title; title from <title>. Chapter title contains " - " and " | ".
        let html = r#"<!DOCTYPE html><html><head><title>Book - Chapter 1 - The Beginning | Scribble Hub</title></head><body>
<div id="chp_raw" class="chp_raw"><p>Content.</p></div></body></html>"#;
        let (title, _) = parse_chapter_page(
            html,
            1,
            "https://www.scribblehub.com/read/123/slug/chapter/1/",
        )?;
        assert_eq!(title, "Book - Chapter 1 - The Beginning");
        let html_pipe = r#"<!DOCTYPE html><html><head><title>Book - Chapter 1 | Part 2 | Scribble Hub</title></head><body>
<div id="chp_raw" class="chp_raw"><p>Content.</p></div></body></html>"#;
        let (title2, _) = parse_chapter_page(
            html_pipe,
            1,
            "https://www.scribblehub.com/read/123/slug/chapter/1/",
        )?;
        assert_eq!(title2, "Book - Chapter 1 | Part 2");
        Ok(())
    }

    #[test]
    fn merge_toc_entries_merges_and_sorts() {
        let page1 = vec![
            (2, "https://example.com/ch2".to_string(), "Ch2".to_string()),
            (1, "https://example.com/ch1".to_string(), "Ch1".to_string()),
        ];
        let page2 = vec![(3, "https://example.com/ch3".to_string(), "Ch3".to_string())];
        let mut all = page1;
        all.extend(page2);
        let merged = merge_toc_entries(all);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].0, 1);
        assert_eq!(merged[0].2, "Ch1");
        assert_eq!(merged[1].0, 2);
        assert_eq!(merged[2].0, 3);
    }

    #[test]
    fn merge_toc_entries_dedupes_by_url() {
        let url = "https://example.com/ch2".to_string();
        let page1 = vec![
            (1, "https://example.com/ch1".to_string(), "Ch1".to_string()),
            (2, url.clone(), "Ch2".to_string()),
        ];
        let page2 = vec![
            (2, url, "Ch2 again".to_string()),
            (3, "https://example.com/ch3".to_string(), "Ch3".to_string()),
        ];
        let mut all = page1;
        all.extend(page2);
        let merged = merge_toc_entries(all);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].2, "Ch2");
    }

    #[test]
    fn next_toc_page_url_returns_some_when_next_link_present() {
        let series_base = Url::parse("https://www.scribblehub.com/series/123/slug/").unwrap();
        let html = r#"<div id="pagination-mesh-toc"><a class="page-link next" href="?toc=2">Next</a></div>"#;
        let url = next_toc_page_url(html, &series_base, Some("https://www.scribblehub.com/series/123/slug/"));
        assert!(url.is_some());
        assert!(url.unwrap().contains("toc=2"));
    }

    #[test]
    fn next_toc_page_url_fallback_finds_toc2_without_next_class() {
        let series_base = Url::parse("https://www.scribblehub.com/series/55539/trouble-with-horns/").unwrap();
        let html = r#"<ul id="pagination-mesh-toc"><li class="active"><a class="current" href="?toc=1#content1">1</a></li><li><a href="?toc=2#content1" class="page-link">2</a></li><li><a href="?toc=3#content1" class="page-link">»</a></li></ul>"#;
        let url = next_toc_page_url(html, &series_base, Some("https://www.scribblehub.com/series/55539/trouble-with-horns/"));
        assert!(url.is_some(), "fallback should find toc=2 when .next class is missing");
        assert!(url.unwrap().contains("toc=2"));
    }

    #[test]
    fn next_toc_page_url_returns_none_when_no_next() {
        let series_base = Url::parse("https://www.scribblehub.com/series/123/slug/").unwrap();
        let html_no_next = r#"<div id="pagination-mesh-toc"></div>"#;
        assert!(next_toc_page_url(html_no_next, &series_base, None).is_none());
        let html_disabled =
            r#"<div id="pagination-mesh-toc"><span class="page-link next">Next</span></div>"#;
        assert!(next_toc_page_url(html_disabled, &series_base, None).is_none());
        let html_hash =
            r##"<div id="pagination-mesh-toc"><a class="page-link next" href="#">Next</a></div>"##;
        assert!(next_toc_page_url(html_hash, &series_base, None).is_none());
    }

    #[test]
    fn fixture_series_metadata_and_toc() -> Result<(), ScraperError> {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };
        let base = Path::new(&manifest_dir).join("scribblehub");
        let series_path = base.join("HP_ The Arcane Thief (LitRPG) _ Scribble Hub.html");

        let series_html = match std::fs::read_to_string(&series_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };

        let (title, author, description, cover_url) = parse_metadata(&series_html)?;
        assert_eq!(title, "HP: The Arcane Thief (LitRPG)");
        assert_eq!(author, "Snollygoster");
        assert!(description.is_some());
        assert!(cover_url.is_some());

        let base_url =
            Url::parse(SCRIBBLEHUB_BASE).map_err(|e| ScraperError::ChapterListParse {
                reason: e.to_string(),
            })?;
        let entries = parse_toc_page(&series_html, &base_url)?;
        assert!(!entries.is_empty());
        assert!(entries[0].1.contains("scribblehub.com"));
        assert!(entries[0].2.starts_with("Chapter "));
        Ok(())
    }

    #[test]
    fn fixture_chapter_parse() -> Result<(), ScraperError> {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };
        let base = Path::new(&manifest_dir).join("scribblehub");
        let chapter_path = base.join(
            "HP_ The Arcane Thief (LitRPG) - Chapter 239_ All Hail, King Axel _ Scribble Hub.html",
        );

        let chapter_html = match std::fs::read_to_string(&chapter_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };

        let (ch_title, body) = parse_chapter_page(
            &chapter_html,
            239,
            "https://www.scribblehub.com/read/862913-hp-the-arcane-thief-litrpg/chapter/1383859/",
        )?;
        assert_eq!(ch_title, "Chapter 239: All Hail, King Axel");
        assert!(!body.is_empty());
        assert!(body.contains("<p>"));
        Ok(())
    }

    /// Fixture test: parse series and chapter from "Immortal Paladin" saved HTML.
    /// Skips if fixture files are not present.
    #[test]
    fn fixture_immortal_paladin_parse() -> Result<(), ScraperError> {
        let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };
        let base = Path::new(&manifest_dir).join("scribblehub");
        let series_path = base.join("Immortal Paladin _ Scribble Hub.html");
        let chapter_path = base.join(
            "Immortal Paladin - Book 1 – Yellow Dragon Festival [REWRITE][Part1] _ Scribble Hub.html",
        );

        let series_html = match std::fs::read_to_string(&series_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };

        let (title, author, description, cover_url) = parse_metadata(&series_html)?;
        assert_eq!(title, "Immortal Paladin");
        assert_eq!(author, "Alfir");
        assert!(description.is_some());
        assert!(cover_url.is_some());

        let base_url =
            Url::parse(SCRIBBLEHUB_BASE).map_err(|e| ScraperError::ChapterListParse {
                reason: e.to_string(),
            })?;
        let entries = parse_toc_page(&series_html, &base_url)?;
        assert!(!entries.is_empty());
        assert!(entries[0].1.contains("scribblehub.com"));

        let chapter_html = match std::fs::read_to_string(&chapter_path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };
        let (ch_title, body) = parse_chapter_page(
            &chapter_html,
            1,
            "https://www.scribblehub.com/read/1414286-immortal-paladin/chapter/2133716/",
        )?;
        assert_eq!(ch_title, "Book 1 – Yellow Dragon Festival [REWRITE][Part1]");
        assert!(!body.is_empty());
        assert!(body.contains("<p>"));
        Ok(())
    }
}
