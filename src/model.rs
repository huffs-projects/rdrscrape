//! Canonical data model for scraped fiction.
//!
//! Shape is defined in OUTPUT_SHAPE.md.
//! The EPUB writer and scrapers use this as the single source of truth.

use serde::{Deserialize, Serialize};

/// Canonical book shape: one story/series.
///
/// See OUTPUT_SHAPE.md. All site adapters produce this shape; the EPUB writer consumes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub title: String,
    pub author: String,
    pub description: Option<String>,
    #[serde(rename = "coverUrl")]
    pub cover_url: Option<String>,
    pub chapters: Vec<Chapter>,
    /// Origin URL for logging/cache. Not in OUTPUT_SHAPE.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

/// One chapter in TOC order.
///
/// See OUTPUT_SHAPE.md. `body` is plain text or minimal HTML (e.g. `<p>...</p>` only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    /// 1-based order from TOC.
    pub index: u32,
    /// Plain text or minimal HTML (e.g. `<p>...</p>` only).
    pub body: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    fn sample_book() -> Book {
        Book {
            title: "Mother of Learning".to_string(),
            author: "nobody103".to_string(),
            description: Some("Zorian is a teenage mage in a time loop...".to_string()),
            cover_url: Some("https://www.royalroad.com/fiction/covers/21220".to_string()),
            chapters: vec![Chapter {
                title: "1. Good Morning Brother".to_string(),
                index: 1,
                body: "<p>The first paragraph of the chapter.</p><p>The second paragraph.</p>"
                    .to_string(),
            }],
            source_url: None,
        }
    }

    #[test]
    fn book_serializes_to_output_shape_json() -> Result<(), Box<dyn Error>> {
        let book = sample_book();
        let json = serde_json::to_string(&book)?;
        assert!(json.contains("\"title\":\"Mother of Learning\""));
        assert!(json.contains("\"author\":\"nobody103\""));
        assert!(json.contains("\"coverUrl\":"));
        assert!(json.contains("\"chapters\":"));
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        let chapters = parsed
            .get("chapters")
            .and_then(|c| c.as_array())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no chapters"))?;
        assert_eq!(chapters.len(), 1);
        let ch = &chapters[0];
        assert_eq!(
            ch.get("title").and_then(|t| t.as_str()),
            Some("1. Good Morning Brother")
        );
        assert_eq!(ch.get("index").and_then(|i| i.as_u64()), Some(1));
        assert!(ch
            .get("body")
            .and_then(|b| b.as_str())
            .map(|s| s.contains("<p>"))
            .unwrap_or(false));
        Ok(())
    }

    /// Round-trip: build Book with one Chapter, serialize to JSON, assert shape matches OUTPUT_SHAPE,
    /// then deserialize back to Book and assert equality.
    #[test]
    fn book_round_trip_json_matches_output_shape() -> Result<(), Box<dyn Error>> {
        let book = sample_book();
        let json = serde_json::to_string(&book)?;
        let value: serde_json::Value = serde_json::from_str(&json)?;

        // OUTPUT_SHAPE: Book has required title, author, chapters; optional description, coverUrl.
        let obj = value.as_object().expect("root must be object");
        assert!(obj.contains_key("title"), "missing title");
        assert!(obj.contains_key("author"), "missing author");
        assert!(obj.contains_key("chapters"), "missing chapters");
        assert_eq!(obj["title"].as_str(), Some("Mother of Learning"));
        assert_eq!(obj["author"].as_str(), Some("nobody103"));
        assert!(obj
            .get("description")
            .map(|d| d.is_string())
            .unwrap_or(true));
        assert!(obj.get("coverUrl").map(|c| c.is_string()).unwrap_or(true));

        let chapters = obj
            .get("chapters")
            .and_then(|c| c.as_array())
            .expect("chapters must be array");
        assert!(!chapters.is_empty());
        for (i, ch) in chapters.iter().enumerate() {
            let ch_obj = ch.as_object().expect("chapter must be object");
            assert!(ch_obj.contains_key("title"), "chapter {} missing title", i);
            assert!(ch_obj.contains_key("index"), "chapter {} missing index", i);
            assert!(ch_obj.contains_key("body"), "chapter {} missing body", i);
            assert!(
                ch_obj["index"].as_u64().unwrap_or(0) >= 1,
                "index must be 1-based"
            );
        }

        let round_tripped: Book = serde_json::from_str(&json)?;
        assert_eq!(round_tripped.title, book.title);
        assert_eq!(round_tripped.author, book.author);
        assert_eq!(round_tripped.description, book.description);
        assert_eq!(round_tripped.cover_url, book.cover_url);
        assert_eq!(round_tripped.chapters.len(), book.chapters.len());
        for (a, b) in round_tripped.chapters.iter().zip(book.chapters.iter()) {
            assert_eq!(a.title, b.title);
            assert_eq!(a.index, b.index);
            assert_eq!(a.body, b.body);
        }
        Ok(())
    }
}
