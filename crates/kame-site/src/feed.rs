//! The RSS feed, with the latest published documents in full.

use std::path::Path;

use kame_core::export::{html_body, Publish};

use crate::config::Config;
use crate::refs::{Images, SiteIndex};
use crate::source::Source;
use crate::Problems;

/// How many documents the feed carries.
const FEED_ITEMS: usize = 10;

/// `text` as XML character data. Characters XML 1.0 can't hold at all are dropped, since no
/// escaping would make them valid, and pasted text can carry them.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if u32::from(c) < 0x20 || matches!(c, '\u{fffe}' | '\u{ffff}') => {}
            c => out.push(c),
        }
    }
    out
}

/// The feed, holding the newest published documents. Rendered apart from their pages, because
/// feed readers don't reliably resolve relative links. There is no `lastBuildDate`, which would
/// change on every build.
pub fn render(config: &Config, sources: &[Source], index: &SiteIndex, site_dir: &Path) -> String {
    let site = &config.url;
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n<channel>\n",
    );
    out.push_str(&format!("<title>{}</title>\n", escape(&config.name)));
    out.push_str(&format!("<link>{}/</link>\n", escape(site)));
    out.push_str(&format!(
        "<description>{}</description>\n",
        escape(&config.description)
    ));
    for source in sources
        .iter()
        .filter(|source| !source.draft)
        .take(FEED_ITEMS)
    {
        let doc_url = source.url();
        // Warnings about images were already reported when building the page.
        let images = Images::resolve(source, site_dir, &mut Problems::default());
        let document_link = |target: &str| {
            index
                .link_url(target, source)
                .map(|url| format!("{site}{url}"))
        };
        let image = |path: &str| images.absolute(path, site, &doc_url);
        let body = html_body(
            &source.doc,
            &Publish {
                document_link: &document_link,
                image: &image,
            },
        );
        let url = format!("{site}{doc_url}");
        out.push_str("<item>\n");
        out.push_str(&format!("<title>{}</title>\n", escape(&source.title)));
        out.push_str(&format!("<link>{}</link>\n", escape(&url)));
        out.push_str(&format!(
            "<guid isPermaLink=\"true\">{}</guid>\n",
            escape(&url)
        ));
        out.push_str(&format!("<pubDate>{}</pubDate>\n", source.date.rfc822()));
        // RSS's own author element wants an email address, which kame doesn't keep.
        if !source.author.is_empty() {
            out.push_str(&format!(
                "<dc:creator>{}</dc:creator>\n",
                escape(&source.author)
            ));
        }
        for tag in &source.tags {
            out.push_str(&format!("<category>{}</category>\n", escape(&tag.name)));
        }
        out.push_str(&format!("<description>{}</description>\n", escape(&body)));
        out.push_str("</item>\n");
    }
    out.push_str("</channel>\n</rss>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Entry;
    use crate::source;
    use crate::testing::{block, metadata, paragraph, temp_dir, write_kame};
    use kame_core::model::{BlockKind, Document, Span, SpanStyle};

    fn config() -> Config {
        Config {
            name: "A & B".to_string(),
            url: "https://e.com".to_string(),
            description: "Things".to_string(),
            author: String::new(),
            publish: Vec::new(),
            drafts: Vec::new(),
        }
    }

    fn make(dir: &Path, slug: &str, date: &str, draft: bool, document: Document) -> Source {
        let path = dir.join(format!("{slug}.kame"));
        write_kame(&path, &document, Some(&metadata(slug, date, &["t one"])));
        let mut problems = Problems::default();
        source::read(
            &Entry {
                slug: slug.to_string(),
                path,
            },
            draft,
            &mut problems,
        )
        .unwrap()
    }

    fn plain(text: &str) -> Document {
        let mut doc = Document::new("D");
        doc.blocks = vec![paragraph(text)];
        doc
    }

    fn items(xml: &str) -> Vec<String> {
        let parsed = roxmltree::Document::parse(xml).unwrap();
        parsed
            .descendants()
            .filter(|node| node.has_tag_name("item"))
            .map(|item| {
                item.children()
                    .find(|node| node.has_tag_name("title"))
                    .and_then(|node| node.text())
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    fn feed(dir: &Path, sources: &[Source]) -> String {
        render(&config(), sources, &SiteIndex::new(sources), dir)
    }

    #[test]
    fn it_is_rss_with_the_channel_filled_in() {
        let dir = temp_dir("feed");
        let xml = feed(
            &dir,
            &[make(&dir, "a", "2026-11-02", false, plain("Words."))],
        );
        let parsed = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(parsed.root_element().attribute("version"), Some("2.0"));
        let text = |name: &str| {
            parsed
                .descendants()
                .find(|n| n.has_tag_name(name))
                .and_then(|n| n.text())
                .unwrap()
                .to_string()
        };
        assert_eq!(text("title"), "A & B");
        assert_eq!(text("link"), "https://e.com/");
        assert_eq!(text("description"), "Things");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_item_has_what_a_reader_needs() {
        let dir = temp_dir("item");
        let xml = feed(
            &dir,
            &[make(&dir, "a", "2026-11-02", false, plain("Words."))],
        );
        assert!(xml.contains("<link>https://e.com/k/a/</link>"), "{xml}");
        assert!(
            xml.contains("<guid isPermaLink=\"true\">https://e.com/k/a/</guid>"),
            "{xml}"
        );
        assert!(
            xml.contains("<pubDate>Mon, 02 Nov 2026 00:00:00 GMT</pubDate>"),
            "{xml}"
        );
        assert!(xml.contains("<category>t one</category>"), "{xml}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_item_names_its_author_when_it_has_one() {
        let dir = temp_dir("author");
        let xml = feed(
            &dir,
            &[make(&dir, "a", "2026-11-02", false, plain("Words."))],
        );
        let parsed = roxmltree::Document::parse(&xml).unwrap();
        let creator = parsed
            .descendants()
            .find(|n| n.tag_name().name() == "creator")
            .and_then(|n| n.text());
        assert_eq!(creator, Some("K. Isom"), "{xml}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_whole_document_is_in_the_item() {
        let dir = temp_dir("whole");
        let xml = feed(
            &dir,
            &[make(&dir, "a", "2026-11-02", false, plain("Every word."))],
        );
        let parsed = roxmltree::Document::parse(&xml).unwrap();
        let item = parsed
            .descendants()
            .find(|n| n.has_tag_name("item"))
            .unwrap();
        let description = item
            .children()
            .find(|n| n.has_tag_name("description"))
            .unwrap();
        assert_eq!(description.text(), Some("<p>Every word.</p>\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_newest_ten_are_in_it() {
        let dir = temp_dir("ten");
        let sources: Vec<Source> = (1..=12)
            .rev()
            .map(|day| {
                make(
                    &dir,
                    &format!("d{day:02}"),
                    &format!("2026-01-{day:02}"),
                    false,
                    plain("x"),
                )
            })
            .collect();
        let found = items(&feed(&dir, &sources));
        assert_eq!(found.len(), 10);
        assert_eq!(found[0], "d12");
        assert_eq!(found[9], "d03");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn drafts_are_never_in_it() {
        let dir = temp_dir("nodrafts");
        let sources = vec![
            make(&dir, "pub", "2026-01-01", false, plain("x")),
            make(&dir, "draft", "2030-01-01", true, plain("x")),
        ];
        assert_eq!(items(&feed(&dir, &sources)), ["pub"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_site_has_an_empty_feed() {
        let dir = temp_dir("emptyfeed");
        assert!(items(&feed(&dir, &[])).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn links_and_images_are_absolute() {
        let dir = temp_dir("absolute");
        std::fs::write(dir.join("a.png"), "x").unwrap();
        let mut doc = Document::new("D");
        let mut linked = paragraph("See b.");
        linked.spans = vec![Span {
            start: 4,
            end: 5,
            style: SpanStyle::DocumentLink {
                target: "b.kame".to_string(),
            },
        }];
        doc.blocks = vec![
            linked,
            block(
                BlockKind::Image {
                    path: "a.png".to_string(),
                    link: None,
                },
                "",
            ),
        ];
        let sources = vec![
            make(&dir, "a", "2026-01-02", false, doc),
            make(&dir, "b", "2026-01-01", false, plain("x")),
        ];
        let xml = feed(&dir, &sources);
        assert!(xml.contains("href=&quot;https://e.com/k/b/&quot;"), "{xml}");
        assert!(
            xml.contains("src=&quot;https://e.com/k/a/a.png&quot;"),
            "{xml}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Review focus: pasted text mustn't break the feed.
    #[test]
    fn awkward_text_still_makes_a_well_formed_feed() {
        let dir = temp_dir("awkwardxml");
        let text = "a ]]> b & <c> \u{1} \u{b} \"d\" 'e'";
        let xml = feed(&dir, &[make(&dir, "a", "2026-01-01", false, plain(text))]);
        let parsed = roxmltree::Document::parse(&xml).expect("well-formed");
        let item = parsed
            .descendants()
            .find(|n| n.has_tag_name("item"))
            .unwrap();
        let description = item
            .children()
            .find(|n| n.has_tag_name("description"))
            .unwrap();
        assert!(description.text().unwrap().contains("]]&gt;"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_same_input_gives_the_same_feed() {
        let dir = temp_dir("samefeed");
        let sources = vec![make(&dir, "a", "2026-01-01", false, plain("x"))];
        assert_eq!(feed(&dir, &sources), feed(&dir, &sources));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
