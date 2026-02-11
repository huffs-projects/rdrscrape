# Scraper output shape

Canonical structure for one book and one chapter. Royal Road and Scribble Hub scrapers should emit this shape so the EPUB step has a single, consistent contract.

## Pseudo-structure

A JSON Schema is available at [schema/book.json](schema/book.json).

```
Book:
  title: string
  author: string
  description?: string
  coverUrl?: string
  chapters: Chapter[]

Chapter:
  title: string
  index: number          // 1-based order from TOC
  body: string           // plain text or minimal HTML (<p>...</p> only)
```

- **Book**: One object per story/series. `description` and `coverUrl` are optional (sites may omit or truncate them).
- **Chapter**: One object per chapter, in TOC order. `body` is either plain text or semantic HTML (paragraphs only) so the EPUB pipeline can wrap it in XHTML.

## Example (one book, one chapter)

```json
{
  "title": "Mother of Learning",
  "author": "nobody103",
  "description": "Zorian is a teenage mage in a time loop...",
  "coverUrl": "https://www.royalroad.com/fiction/covers/21220",
  "chapters": [
    {
      "title": "1. Good Morning Brother",
      "index": 1,
      "body": "<p>The first paragraph of the chapter.</p><p>The second paragraph.</p>"
    }
  ]
}
```

Same shape works for Scribble Hub or any other source; only the origin URL/site differs. The canonical struct is consumed by the EPUB writer, JSON output, and single-file HTML, Markdown, and plain-text writers (see `--format` in the CLI).

## JSON Schema

The canonical JSON Schema (draft-07) for this shape is in [schema/book.json](schema/book.json). Use it to validate `--format json` output or to generate types for other tools.
