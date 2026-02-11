# rdrscrape

CLI scraper for Royal Road and Scribble Hub fiction. Output formats: EPUB, JSON, single-file HTML, Markdown, or plain text.

## Installation

### Homebrew (MacOS)
```bash
brew tap huffs-projects/rdrscrape
brew install rdrscrape
```
(working on bottled binary so you don't have to get the whole rust toolchain the first time)
### Build from Source
```bash
cargo install --path
```
(just make sure `~/.cargo/bin` is in your PATH)
## Build

```bash
cargo build
```

Release build:

```bash
cargo build --release
```


## Usage

```bash
rdrscrape <URL> [-o path] [--format epub|json|html|markdown|text]
```

Full list of flags and config keys: see **Flags and configuration** below. Run `rdrscrape --help` for option summaries. A man page is provided in `man/rdrscrape.1` (install to your man path, or view with `man man/rdrscrape.1` when run from the project root).

**Format and output**: `--format` chooses the output format (default `epub`). Extensions: `.epub`, `.json`, `.html`, `.md`, `.txt`. If `-o` is omitted, output is `{output_dir}/{sanitized-title}.{ext}` where `output_dir` is from config or `.`.

**Examples**

- Royal Road: `rdrscrape https://www.royalroad.com/fiction/21220/mother-of-learning`
- Scribble Hub: `rdrscrape https://www.scribblehub.com/series/862913/hp-the-arcane-thief-litrpg/`
- Custom output: `rdrscrape "https://www.royalroad.com/fiction/21220/mother-of-learning" -o mol.epub`
- Single HTML file: `rdrscrape <URL> --format html -o book.html`
- Markdown: `rdrscrape <URL> --format markdown` (writes `./{title}.md`)
- Plain text: `rdrscrape <URL> --format text`
- JSON (canonical Book only): `rdrscrape <URL> --format json -o book.json`
- EPUB 2: `rdrscrape <URL> --epub-2`
- Quiet (no progress): `rdrscrape <URL> -q`
- Override site: `rdrscrape <URL> --site royalroad`
- EPUB 3 with NCX (legacy readers): `rdrscrape <URL> --ncx`
- Locked chapters (Royal Road): `rdrscrape <URL> --locked-chapters skip` (default), `placeholder`, or `fail`
- Empty chapters: `rdrscrape <URL> --empty-chapters skip` (default), `placeholder`, or `fail` (chapters with no content or unparseable)
- Config overrides: `rdrscrape <URL> --user-agent "..." --delay 3 --timeout 60`
- Dry run: `rdrscrape <URL> --dry-run` (resolve site, fetch TOC only, print chapter count and output path; no files written)
- Validate EPUB: `rdrscrape <URL> --validate` (after writing EPUB, run epubcheck; requires epubcheck on PATH)

## Flags and configuration

All flags and config keys in one place. Optional config file: search order (1) `./rdrscrape.toml`, (2) `$XDG_CONFIG_HOME/rdrscrape/config.toml` (or `~/.config/rdrscrape/config.toml`). Missing file is not an error. **CLI flags override config** where both apply.

### CLI options

| Option | Description | Default |
|--------|-------------|---------|
| `URL` | Story or series URL (Royal Road fiction page or Scribble Hub series page) | (required) |
| `-o`, `--output <PATH>` | Output path | `{output_dir}/{sanitized-title}.{ext}` |
| `--format <FORMAT>` | Output format: epub, json, html, markdown, text | epub |
| `--site <SITE>` | Override site detection: royalroad, scribblehub | from URL |
| `--epub-2` | Generate EPUB 2 instead of EPUB 3 (format=epub only) | false |
| `-q`, `--quiet` | Suppress progress output (errors only) | false |
| `--verbose` | Print verbose error chain | false |
| `--ncx` | Include toc.ncx in EPUB 3 for legacy readers | false |
| `--chapters <FROM>-<TO>` | Scrape only chapters in range (1-based inclusive), e.g. 1-10 | all |
| `--resume <PATH>` | Resume from partial JSON; fetch only missing chapters | (none) |
| `--locked-chapters <MODE>` | Royal Road locked chapters: skip, placeholder, fail | skip |
| `--empty-chapters <MODE>` | Empty or unparseable chapter: skip, placeholder, fail | skip |
| `--user-agent <STRING>` | HTTP User-Agent (overrides config) | (from config or built-in) |
| `--delay <SECS>` | Delay between requests in seconds (overrides config) | 2 |
| `--timeout <SECS>` | Request timeout in seconds (overrides config) | 30 |
| `--dry-run` | Fetch TOC only; print chapter count and output path; no files written | false |
| `--validate` | Run epubcheck on generated EPUB (epubcheck on PATH) | false |

### Config file keys (TOML)

| Key | Description | Default |
|-----|-------------|---------|
| `output_dir` | Default output directory when `-o` is omitted | `.` |
| `user_agent` | HTTP User-Agent | (built-in) |
| `request_delay_secs` | Delay between requests in seconds | 2 |
| `timeout_secs` | Request timeout in seconds | 30 |
| `toc_page` | Include visible TOC page after cover in EPUB | true |
| `retry_count` | Number of HTTP attempts for transient failures | 3 |
| `retry_backoff_secs` | Delay before each retry, array in seconds (e.g. `[1, 2, 4]`); length `retry_count - 1` | [1, 2, 4] |
| `empty_chapters` | Empty/missing chapter body: skip, placeholder, fail | skip |

Example `rdrscrape.toml`:

```toml
output_dir = "."
user_agent = "Mozilla/5.0 (compatible; rdrscrape/0.1; +https://github.com/rdrscrape)"
request_delay_secs = 2
timeout_secs = 30
# toc_page = false   # set to disable TOC page in EPUB
# retry_count = 5
# retry_backoff_secs = [1, 2, 4, 8]
# empty_chapters = "placeholder"   # skip (default), placeholder, or fail
```

**Scope**: Authentication and premium chapter handling are unchanged (see **Known edge cases**).

## Dependencies

- **clap** – CLI parsing
- **reqwest** (blocking, cookies) – HTTP
- **scraper** – HTML/CSS selectors, plain-text extraction
- **serde**, **serde_json** – canonical model, JSON output
- **thiserror**, **anyhow** – errors
- **zip** – EPUB archive
- **html2md** – HTML to Markdown for `--format markdown`

## Exit codes

- **0** – success
- **1** – invalid input (URL, site, output path)
- **2** – scraper failure (network, parse, site)
- **3** – EPUB or format write failure

Use `--verbose` to print the error cause chain.

## Stability and behavior

- **Cover**: If the cover image URL is set but the fetch fails (network, HTTP error, or read error), a title-only cover page (book title and author) is generated instead; the EPUB is still written. If no cover URL is set, no cover page is included.
- **EPUB 3 NCX**: By default, EPUB 3 output does not include `toc.ncx`. Use `--ncx` to include it for legacy readers. EPUB 2 always includes NCX.
- **TOC page**: A visible table-of-contents page is inserted after the cover by default. Disable with `toc_page = false` in config.
- **Request delay**: 2 seconds between requests (configurable via config file or `--delay`).
- **Timeout**: 30 seconds per request (configurable via config file or `--timeout`).
- **Retries**: Transient failures (timeout, connection errors, HTTP 5xx, 429) are retried; default 3 attempts with backoff 1s, 2s, 4s. Configure via `retry_count` and `retry_backoff_secs` in config. Non-retryable errors (e.g. 4xx except 429) are not retried.
- **EPUB validation**: Use `--validate` to run [epubcheck](https://github.com/w3c/epubcheck) on the generated EPUB after write. Exit code 3 if validation fails or if epubcheck is not on PATH.
- **Rate limiting**: Default delay is conservative; respect site terms of use.
- **Cloudflare / captcha**: Not handled. Scripted access may be blocked; see **Known edge cases** below.

## Known edge cases

Edge cases and gotchas when scraping Royal Road and Scribble Hub.

**Royal Road**: Cloudflare and cookies (sessions use cookies; scripted fetches may be blocked). Locked/premium chapters: `window.chapters` entries with `isUnlocked: false`; default is skip; use `--locked-chapters placeholder` or `fail` as needed. Chapter body uses obfuscated/hashed class names—select by container and tag (`div.chapter-inner.chapter-content p`). Prefer `window.chapters` for full TOC (visible TOC is paginated). Chapter title: prefer `h1.font-white.break-word` or `og:title`/`<title>`. Description may be truncated ("show more"). Chapter URLs in `window.chapters` are relative; resolve against base domain.

**Scribble Hub**: Use the **series page** TOC only (in-chapter TOC is JS-loaded, not reliable). Extract only from `#chp_raw`; exclude ads/comments in `#chp_contents`. Site is WordPress-based; prefer IDs and JSON-LD. TOC can be paginated (`?toc=N`); follow next link until absent, then merge and deduplicate by chapter URL. "Next" on last chapter may be `href="#"` or disabled. Description may be truncated.

**General**: Title parsing (e.g. "ChapterTitle - FictionTitle") can break if the title itself contains `" - "` or `" | "`. Empty or non-standard pages (404s, paywalls) may return empty or unexpected HTML; handle missing containers and empty body gracefully. Use UTF-8 for all text so non-ASCII (curly quotes, accents) is preserved for EPUB.

## Connection or network errors

If you see **"Network error: could not reach &lt;url&gt;"** or **"error sending request"**, the connection to the site failed before a response was received. Common causes:

- **Site or Cloudflare blocking scripted access** – Royal Road uses Cloudflare; some requests from scripts or data centers are blocked. Try opening the same URL in a browser; if it loads there, the site may be rejecting the scraper. See **Known edge cases** above. Retry later or from a different network.
- **Timeout or unreachable host** – Slow or flaky network, or the site is down. Retries (3 attempts with backoff) are automatic; if all fail, try again later.
- **Local firewall or DNS** – Outbound HTTPS may be restricted, or DNS may not resolve.

Use **`--verbose`** to print the full error chain (e.g. connection refused, TLS error, timeout) to see the underlying cause.

## References

- [OUTPUT_SHAPE.md](OUTPUT_SHAPE.md) – canonical Book/Chapter
- [schema/book.json](schema/book.json) – JSON Schema for Book/Chapter (draft-07)
- [ERROR_HANDLING.md](ERROR_HANDLING.md) – exit codes, messages
