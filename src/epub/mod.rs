//! EPUB writer. Consumes canonical `Book` and writes EPUB 2 or EPUB 3 (mimetype, container, OPF, nav/NCX, chapters).

use crate::model::Book;
use crate::scraper::PoliteClient;
use std::io::{Seek, Write};
use std::path::Path;
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const CONTAINER_XML: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n  <rootfiles>\n    <rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n  </rootfiles>\n</container>";

/// EPUB format version.
///
/// Default is EPUB 3 (OPF 3.0, nav.xhtml, HTML5 chapters). Use `Epub2` for legacy readers (OPF 2.0, NCX, XHTML 1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpubVersion {
    /// EPUB 3: OPF 3.0, nav.xhtml, HTML5 chapters. Optional toc.ncx for compatibility.
    Epub3,
    /// EPUB 2: OPF 2.0, toc.ncx only, XHTML 1.1 chapters.
    Epub2,
}

/// Errors from the EPUB writer.
///
/// Maps to CLI exit code 3. See ERROR_HANDLING.md 2.4 for messages and behavior.
#[derive(Debug, Error)]
pub enum EpubError {
    #[error("Cannot write EPUB: book title is empty.")]
    EmptyTitle,

    #[error("Cannot write EPUB: book author is empty.")]
    EmptyAuthor,

    #[error("Cannot write EPUB: book has no chapters.")]
    NoChapters,

    #[error("Cannot write EPUB: {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to create EPUB file: {path}: {source}")]
    CreateFile {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to write EPUB archive: {0}")]
    Zip(#[from] zip::result::ZipError),
}

impl From<std::io::Error> for EpubError {
    fn from(e: std::io::Error) -> Self {
        EpubError::Zip(zip::result::ZipError::Io(e))
    }
}

const MIMETYPE: &[u8] = b"application/epub+zip";
const OEBPS_PREFIX: &str = "OEBPS/";

/// Result of cover handling: none, title-only (fetch failed), or image.
#[derive(Debug)]
enum CoverOutcome {
    NoCover,
    TitleOnly,
    Image { data: Vec<u8>, ext: &'static str },
}

/// Write a canonical [Book](crate::model::Book) to an EPUB file.
///
/// Fetches cover image using `client` if `book.cover_url` is set. On cover fetch failure,
/// emits a title-only cover page (no image) and warns to stderr; does not fail the write.
/// Set `epub3_include_ncx` to true to include toc.ncx in EPUB 3 for legacy readers.
/// Set `include_toc_page` to true to insert a visible table-of-contents page after the cover. Output is intended to pass epubcheck.
pub fn write_epub(
    book: &Book,
    path: &Path,
    version: EpubVersion,
    epub3_include_ncx: bool,
    include_toc_page: bool,
    client: &mut PoliteClient,
) -> Result<(), EpubError> {
    validate_book(book)?;

    let path = path.to_path_buf();
    let file = std::fs::File::create(&path).map_err(|e| EpubError::CreateFile {
        path: path.clone(),
        source: e,
    })?;
    let mut zip = ZipWriter::new(file);

    let options_stored = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o644);
    let options_deflate = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // 1. Mimetype first, uncompressed (required by EPUB spec)
    zip.start_file("mimetype", options_stored)?;
    zip.write_all(MIMETYPE)?;

    // 2. Container
    zip.start_file("META-INF/container.xml", options_deflate)?;
    zip.write_all(CONTAINER_XML)?;

    // Cover: try to fetch; on failure use title-only cover page
    let cover = fetch_cover(book, client);

    match version {
        EpubVersion::Epub3 => {
            write_opf3(
                book,
                &cover,
                epub3_include_ncx,
                include_toc_page,
                &mut zip,
                options_deflate,
            )?;
            write_nav_xhtml(book, &mut zip, options_deflate)?;
            if epub3_include_ncx {
                write_ncx(book, &mut zip, options_deflate)?;
            }
            write_cover_xhtml(book, &cover, &mut zip, options_deflate)?;
            if include_toc_page {
                write_toc_page_xhtml(book, &mut zip, options_deflate)?;
            }
            write_chapters_html5(book, &mut zip, options_deflate)?;
        }
        EpubVersion::Epub2 => {
            write_opf2(book, &cover, include_toc_page, &mut zip, options_deflate)?;
            write_ncx(book, &mut zip, options_deflate)?;
            write_cover_xhtml(book, &cover, &mut zip, options_deflate)?;
            if include_toc_page {
                write_toc_page_xhtml(book, &mut zip, options_deflate)?;
            }
            write_chapters_xhtml11(book, &mut zip, options_deflate)?;
        }
    }

    if let CoverOutcome::Image { data, ext } = &cover {
        let name = format!("{}images/cover.{}", OEBPS_PREFIX, ext);
        zip.start_file(name, options_deflate)?;
        zip.write_all(data)?;
    }

    zip.finish()?;
    Ok(())
}

fn validate_book(book: &Book) -> Result<(), EpubError> {
    if book.title.trim().is_empty() {
        return Err(EpubError::EmptyTitle);
    }
    if book.author.trim().is_empty() {
        return Err(EpubError::EmptyAuthor);
    }
    if book.chapters.is_empty() {
        return Err(EpubError::NoChapters);
    }
    Ok(())
}

/// Fetch cover image. On failure (or no URL), returns TitleOnly so a title-only cover page is still emitted when a URL was set.
fn fetch_cover(book: &Book, client: &mut PoliteClient) -> CoverOutcome {
    let url = match &book.cover_url {
        Some(u) if !u.is_empty() => u.as_str(),
        _ => return CoverOutcome::NoCover,
    };
    let response = match client.get_with_retry(url) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Cover image could not be fetched ({}): {}. Using title-only cover page.",
                url, e
            );
            return CoverOutcome::TitleOnly;
        }
    };
    if !response.status().is_success() {
        eprintln!(
            "Cover image could not be fetched (HTTP {}): {}. Using title-only cover page.",
            response.status().as_u16(),
            url
        );
        return CoverOutcome::TitleOnly;
    }
    let ext = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            if ct.contains("jpeg") || ct.contains("jpg") {
                "jpg"
            } else {
                "png"
            }
        })
        .unwrap_or("png");
    match response.bytes() {
        Ok(b) => CoverOutcome::Image {
            data: b.to_vec(),
            ext,
        },
        Err(e) => {
            eprintln!(
                "Cover image could not be read: {}. Using title-only cover page.",
                e
            );
            CoverOutcome::TitleOnly
        }
    }
}

fn identifier(book: &Book) -> String {
    book.source_url
        .as_deref()
        .unwrap_or("urn:rdrscrape:book")
        .to_string()
}

fn write_opf3(
    book: &Book,
    cover: &CoverOutcome,
    include_ncx: bool,
    include_toc_page: bool,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    let id = xml_escape(&identifier(book));
    let title = xml_escape(&book.title);
    let creator = xml_escape(&book.author);
    let description = book
        .description
        .as_ref()
        .map(|d| xml_escape(d))
        .unwrap_or_default();

    let mut manifest = String::from(
        r#"<item id="content-opf" href="content.opf" media-type="application/oebps-package+xml"/>
  <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
"#,
    );
    if include_ncx {
        manifest.push_str(
            r#"  <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
"#,
        );
    }
    let has_cover_page = !matches!(cover, CoverOutcome::NoCover);
    if let CoverOutcome::Image { ext, .. } = cover {
        manifest.push_str(&format!(
            r#"  <item id="cover-img" href="images/cover.{}" media-type="{}"/>
"#,
            ext,
            cover_media_type(ext)
        ));
    }
    if has_cover_page {
        manifest.push_str(
            r#"  <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
"#,
        );
    }
    if include_toc_page {
        manifest.push_str(
            r#"  <item id="toc-page" href="toc.xhtml" media-type="application/xhtml+xml"/>
"#,
        );
    }
    for (i, _) in book.chapters.iter().enumerate() {
        manifest.push_str(&format!(
            r#"  <item id="chapter-{}" href="chapter-{}.xhtml" media-type="application/xhtml+xml"/>
"#,
            i + 1,
            i + 1
        ));
    }

    // Spine: reading order only (cover, optional toc page, then chapters). Nav is not in spine.
    let mut spine = String::new();
    if has_cover_page {
        spine.push_str(r#"  <itemref idref="cover"/>"#);
    }
    if include_toc_page {
        if !spine.is_empty() {
            spine.push_str("\n  ");
        }
        spine.push_str(r#"<itemref idref="toc-page"/>"#);
    }
    for (i, _) in book.chapters.iter().enumerate() {
        if !spine.is_empty() {
            spine.push_str("\n  ");
        }
        spine.push_str(&format!("<itemref idref=\"chapter-{}\"/>", i + 1));
    }
    if spine.is_empty() {
        spine.push_str(r#"  <itemref idref="chapter-1"/>"#);
    }

    let guide = if has_cover_page {
        r#"  <reference type="cover" href="cover.xhtml" title="Cover"/>"#
    } else {
        ""
    };

    let opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="3.0"
  xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">{id}</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:creator>{creator}</dc:creator>
    <dc:language>en</dc:language>
    {description_el}
  </metadata>
  <manifest>
{manifest}  </manifest>
  <spine>
{spine}
  </spine>
  <guide>
{guide}
  </guide>
</package>
"#,
        id = id,
        title = title,
        creator = creator,
        description_el = if description.is_empty() {
            String::new()
        } else {
            format!("    <dc:description>{}</dc:description>", description)
        },
        manifest = manifest,
        spine = spine,
        guide = guide
    );

    zip.start_file(format!("{}content.opf", OEBPS_PREFIX), options)?;
    zip.write_all(opf.as_bytes())?;
    Ok(())
}

fn write_opf2(
    book: &Book,
    cover: &CoverOutcome,
    include_toc_page: bool,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    let id = xml_escape(&identifier(book));
    let title = xml_escape(&book.title);
    let creator = xml_escape(&book.author);
    let description = book
        .description
        .as_ref()
        .map(|d| xml_escape(d))
        .unwrap_or_default();

    let mut manifest = String::from(
        r#"<item id="content-opf" href="content.opf" media-type="application/oebps-package+xml"/>
  <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
"#,
    );
    let has_cover_page = !matches!(cover, CoverOutcome::NoCover);
    if let CoverOutcome::Image { ext, .. } = cover {
        manifest.push_str(&format!(
            r#"  <item id="cover-img" href="images/cover.{}" media-type="{}"/>
"#,
            ext,
            cover_media_type(ext)
        ));
    }
    if has_cover_page {
        manifest.push_str(
            r#"  <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
"#,
        );
    }
    if include_toc_page {
        manifest.push_str(
            r#"  <item id="toc-page" href="toc.xhtml" media-type="application/xhtml+xml"/>
"#,
        );
    }
    for (i, _) in book.chapters.iter().enumerate() {
        manifest.push_str(&format!(
            r#"  <item id="chapter-{}" href="chapter-{}.xhtml" media-type="application/xhtml+xml"/>
"#,
            i + 1,
            i + 1
        ));
    }

    // EPUB 2 spine: toc="ncx" references manifest; spine is cover, optional toc page, then chapters.
    let mut spine = String::new();
    if has_cover_page {
        spine.push_str(r#"  <itemref idref="cover"/>"#);
    }
    if include_toc_page {
        if !spine.is_empty() {
            spine.push_str("\n  ");
        }
        spine.push_str(r#"<itemref idref="toc-page"/>"#);
    }
    for (i, _) in book.chapters.iter().enumerate() {
        if !spine.is_empty() {
            spine.push_str("\n  ");
        }
        spine.push_str(&format!("<itemref idref=\"chapter-{}\"/>", i + 1));
    }
    if spine.is_empty() {
        spine.push_str(r#"  <itemref idref="chapter-1"/>"#);
    }

    let guide = if has_cover_page {
        r#"  <reference type="cover" href="cover.xhtml" title="Cover"/>"#
    } else {
        ""
    };

    let opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="book-id" version="2.0"
  xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">{id}</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:creator>{creator}</dc:creator>
    <dc:language>en</dc:language>
    {description_el}
  </metadata>
  <manifest>
{manifest}  </manifest>
  <spine toc="ncx">
{spine}
  </spine>
  <guide>
{guide}
  </guide>
</package>
"#,
        id = id,
        title = title,
        creator = creator,
        description_el = if description.is_empty() {
            String::new()
        } else {
            format!("    <dc:description>{}</dc:description>", description)
        },
        manifest = manifest,
        spine = spine,
        guide = guide
    );

    zip.start_file(format!("{}content.opf", OEBPS_PREFIX), options)?;
    zip.write_all(opf.as_bytes())?;
    Ok(())
}

fn cover_media_type(ext: &str) -> &'static str {
    match ext {
        "jpg" => "image/jpeg",
        _ => "image/png",
    }
}

fn write_nav_xhtml(
    book: &Book,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    let mut nav_links = String::new();
    for (i, ch) in book.chapters.iter().enumerate() {
        let title = html_escape_attr(&ch.title);
        nav_links.push_str(&format!(
            r#"    <li><a href="chapter-{}.xhtml">{}</a></li>
"#,
            i + 1,
            title
        ));
    }
    let nav = format!(
        r#"<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head>
  <meta charset="UTF-8"/>
  <title>Table of Contents</title>
</head>
<body>
  <nav epub:type="toc">
    <h1>Contents</h1>
    <ol>
{}
    </ol>
  </nav>
</body>
</html>
"#,
        nav_links
    );
    zip.start_file(format!("{}nav.xhtml", OEBPS_PREFIX), options)?;
    zip.write_all(nav.as_bytes())?;
    Ok(())
}

/// Writes a visible table-of-contents page (toc.xhtml) for the reading spine. Placed after the cover.
fn write_toc_page_xhtml(
    book: &Book,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    let mut items = String::new();
    for (i, ch) in book.chapters.iter().enumerate() {
        let title = html_escape_attr(&ch.title);
        items.push_str(&format!(
            r#"    <li><a href="chapter-{}.xhtml">{}</a></li>
"#,
            i + 1,
            title
        ));
    }
    let toc_xhtml = format!(
        r#"<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <meta charset="UTF-8"/>
  <title>Table of Contents</title>
</head>
<body>
  <h1>Table of Contents</h1>
  <ol>
{}
  </ol>
</body>
</html>
"#,
        items
    );
    zip.start_file(format!("{}toc.xhtml", OEBPS_PREFIX), options)?;
    zip.write_all(toc_xhtml.as_bytes())?;
    Ok(())
}

fn write_ncx(
    book: &Book,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    let title = xml_escape(&book.title);
    let mut nav_points = String::new();
    for (i, ch) in book.chapters.iter().enumerate() {
        let label = xml_escape(&ch.title);
        nav_points.push_str(&format!(
            r#"    <navPoint id="navpoint-{}" playOrder="{}">
      <navLabel><text>{}</text></navLabel>
      <content src="chapter-{}.xhtml"/>
    </navPoint>
"#,
            i + 1,
            i + 1,
            label,
            i + 1
        ));
    }
    let ncx = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="{}"/>
  </head>
  <docTitle>
    <text>{}</text>
  </docTitle>
  <navMap>
{}
  </navMap>
</ncx>
"#,
        xml_escape(&identifier(book)),
        title,
        nav_points
    );
    zip.start_file(format!("{}toc.ncx", OEBPS_PREFIX), options)?;
    zip.write_all(ncx.as_bytes())?;
    Ok(())
}

fn write_cover_xhtml(
    book: &Book,
    cover: &CoverOutcome,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    let body = match cover {
        CoverOutcome::NoCover => return Ok(()),
        CoverOutcome::TitleOnly => {
            let title = html_escape_attr(&book.title);
            let author = html_escape_attr(&book.author);
            format!(
                r#"  <div style="text-align: center; font-family: serif; margin-top: 3em;">
    <h1 style="font-size: 1.5em;">{}</h1>
    <p style="margin-top: 1em;">{}</p>
  </div>"#,
                title, author
            )
        }
        CoverOutcome::Image { ext, .. } => format!(
            r#"  <div style="text-align: center;">
    <img src="images/cover.{}" alt="Cover" style="max-width: 100%; height: auto;"/>
  </div>"#,
            ext
        ),
    };
    let cover_xhtml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <meta charset="UTF-8"/>
  <title>Cover</title>
</head>
<body>
{}
</body>
</html>
"#,
        body
    );
    zip.start_file(format!("{}cover.xhtml", OEBPS_PREFIX), options)?;
    zip.write_all(cover_xhtml.as_bytes())?;
    Ok(())
}

fn write_chapters_html5(
    book: &Book,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    for (i, ch) in book.chapters.iter().enumerate() {
        let title = html_escape_attr(&ch.title);
        let body = &ch.body;
        let html = format!(
            r#"<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <meta charset="UTF-8"/>
  <title>{}</title>
</head>
<body>
{}
</body>
</html>
"#,
            title, body
        );
        let name = format!("{}chapter-{}.xhtml", OEBPS_PREFIX, i + 1);
        zip.start_file(name, options)?;
        zip.write_all(html.as_bytes())?;
    }
    Ok(())
}

fn write_chapters_xhtml11(
    book: &Book,
    zip: &mut ZipWriter<impl Write + Seek>,
    options: SimpleFileOptions,
) -> Result<(), EpubError> {
    for (i, ch) in book.chapters.iter().enumerate() {
        let title = xml_escape(&ch.title);
        let body = &ch.body;
        let html = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <meta charset="UTF-8"/>
  <title>{}</title>
</head>
<body>
{}
</body>
</html>
"#,
            title, body
        );
        let name = format!("{}chapter-{}.xhtml", OEBPS_PREFIX, i + 1);
        zip.start_file(name, options)?;
        zip.write_all(html.as_bytes())?;
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Chapter;
    use std::io::Read;
    use zip::read::ZipArchive;

    fn minimal_book() -> Book {
        Book {
            title: "Test Book".to_string(),
            author: "Test Author".to_string(),
            description: None,
            cover_url: None,
            chapters: vec![Chapter {
                title: "Chapter 1".to_string(),
                index: 1,
                body: "<p>First paragraph.</p>".to_string(),
            }],
            source_url: None,
        }
    }

    #[test]
    fn validate_book_rejects_empty_title() {
        let mut book = minimal_book();
        book.title.clear();
        let path = std::env::temp_dir().join("rdrscrape_epub_void.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        let result = write_epub(&book, &path, EpubVersion::Epub3, false, true, &mut client);
        assert!(matches!(result, Err(EpubError::EmptyTitle)));
    }

    #[test]
    fn validate_book_rejects_empty_author() {
        let mut book = minimal_book();
        book.author.clear();
        let path = std::env::temp_dir().join("rdrscrape_epub_void.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        let result = write_epub(&book, &path, EpubVersion::Epub3, false, true, &mut client);
        assert!(matches!(result, Err(EpubError::EmptyAuthor)));
    }

    #[test]
    fn validate_book_rejects_no_chapters() {
        let mut book = minimal_book();
        book.chapters.clear();
        let path = std::env::temp_dir().join("rdrscrape_epub_void.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        let result = write_epub(&book, &path, EpubVersion::Epub3, false, true, &mut client);
        assert!(matches!(result, Err(EpubError::NoChapters)));
    }

    #[test]
    fn write_epub_epub3_no_cover_produces_valid_zip() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_epub_test_epub3.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        write_epub(&book, &path, EpubVersion::Epub3, false, true, &mut client).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut zip = ZipArchive::new(file).unwrap();
        let names: Vec<String> = zip.file_names().map(String::from).collect();
        assert!(names.contains(&"mimetype".to_string()));
        assert!(names.contains(&"META-INF/container.xml".to_string()));
        assert!(names.contains(&"OEBPS/content.opf".to_string()));
        assert!(names.contains(&"OEBPS/nav.xhtml".to_string()));
        assert!(zip.by_name("OEBPS/chapter-1.xhtml").is_ok());
        assert!(!names.iter().any(|n| n == "OEBPS/toc.ncx"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_epub_epub3_with_ncx_includes_toc_ncx() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_epub_test_epub3_ncx.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        write_epub(&book, &path, EpubVersion::Epub3, true, true, &mut client).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let zip = ZipArchive::new(file).unwrap();
        let names: Vec<String> = zip.file_names().map(String::from).collect();
        assert!(names.contains(&"OEBPS/toc.ncx".to_string()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_epub_epub2_no_cover_produces_valid_zip() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_epub_test_epub2.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        write_epub(&book, &path, EpubVersion::Epub2, false, true, &mut client).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut zip = ZipArchive::new(file).unwrap();
        let names: Vec<String> = zip.file_names().map(String::from).collect();
        assert!(names.contains(&"mimetype".to_string()));
        assert!(names.contains(&"META-INF/container.xml".to_string()));
        assert!(names.contains(&"OEBPS/content.opf".to_string()));
        assert!(names.contains(&"OEBPS/toc.ncx".to_string()));
        assert!(zip.by_name("OEBPS/chapter-1.xhtml").is_ok());
        let mut opf = zip.by_name("OEBPS/content.opf").unwrap();
        let mut opf_content = String::new();
        opf.read_to_string(&mut opf_content).unwrap();
        assert!(opf_content.contains("package") && opf_content.contains("2.0"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_epub_toc_page_false_omits_toc_xhtml() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_epub_test_no_toc_page.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        write_epub(&book, &path, EpubVersion::Epub3, false, false, &mut client).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut zip = ZipArchive::new(file).unwrap();
        let names: Vec<String> = zip.file_names().map(String::from).collect();
        assert!(!names.iter().any(|n| n == "OEBPS/toc.xhtml"));
        let mut opf = zip.by_name("OEBPS/content.opf").unwrap();
        let mut opf_content = String::new();
        opf.read_to_string(&mut opf_content).unwrap();
        assert!(!opf_content.contains("toc-page"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_epub_toc_page_true_includes_toc_xhtml() {
        let book = minimal_book();
        let path = std::env::temp_dir().join("rdrscrape_epub_test_with_toc_page.epub");
        let mut client = crate::PoliteClient::new().unwrap();
        write_epub(&book, &path, EpubVersion::Epub3, false, true, &mut client).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let mut zip_archive = ZipArchive::new(file).unwrap();
        let names: Vec<String> = zip_archive.file_names().map(String::from).collect();
        assert!(names.contains(&"OEBPS/toc.xhtml".to_string()));
        let mut opf = zip_archive.by_name("OEBPS/content.opf").unwrap();
        let mut opf_content = String::new();
        opf.read_to_string(&mut opf_content).unwrap();
        assert!(opf_content.contains("toc-page") && opf_content.contains("toc.xhtml"));
        std::fs::remove_file(&path).ok();
    }
}
