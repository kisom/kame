//! A reference written out in IEEE format, as runs of text so that HTML and Markdown can each
//! style them. Field text is kept exactly as typed: nothing is abbreviated, escaped or recased.

use super::{Kind, Reference};

/// How a run of an entry is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStyle {
    Plain,
    Italic,
    /// A web address, which an export may make a link.
    Url,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: RunStyle,
}

/// More names than this become the first and *et al.*
const MOST_NAMES: usize = 6;

/// What comes after a piece of an entry: `mid` if another piece follows and `end` if it is
/// the last. A leading `.` is dropped where the text already ends with one.
#[derive(Clone, Copy)]
struct After {
    mid: &'static str,
    end: &'static str,
}

const NOTHING: After = After { mid: "", end: "" };
const SPACE: After = After { mid: " ", end: "." };
const COMMA: After = After {
    mid: ", ",
    end: ".",
};
const COLON: After = After {
    mid: ": ",
    end: ".",
};
const PERIOD: After = After {
    mid: ". ",
    end: ".",
};
/// Closes a title in quotes, with the punctuation inside them.
const QUOTE: After = After {
    mid: ",” ",
    end: ".”",
};
/// Closes a web page's title, which is followed by a sentence, not a comma.
const SENTENCE_QUOTE: After = After {
    mid: ".” ",
    end: ".”",
};

impl After {
    /// The same, but ending a sentence, for what comes before `[Online]`.
    fn ending_sentence(self) -> After {
        if self.end.ends_with('”') {
            SENTENCE_QUOTE
        } else {
            PERIOD
        }
    }
}

struct Builder {
    runs: Vec<Run>,
    after: After,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            runs: Vec::new(),
            after: NOTHING,
        }
    }

    fn ends_with_period(&self) -> bool {
        self.runs.last().is_some_and(|run| run.text.ends_with('.'))
    }

    fn punctuation(&mut self, text: &'static str) {
        let text = match text.strip_prefix('.') {
            Some(rest) if self.ends_with_period() => rest,
            _ => text,
        };
        if !text.is_empty() {
            self.runs.push(Run {
                text: text.to_string(),
                style: RunStyle::Plain,
            });
        }
    }

    fn push(&mut self, text: &str, style: RunStyle, after: After) {
        self.punctuation(self.after.mid);
        self.runs.push(Run {
            text: text.to_string(),
            style,
        });
        self.after = after;
    }

    fn plain(&mut self, text: &str, after: After) {
        self.push(text, RunStyle::Plain, after);
    }

    fn italic(&mut self, text: &str, after: After) {
        self.push(text, RunStyle::Italic, after);
    }

    /// A title in quotes.
    fn quoted(&mut self, title: &str, after: After) {
        if title.is_empty() {
            return;
        }
        // A title that ends in a period already has its punctuation, so no comma follows it.
        let after = if title.ends_with('.') && after.mid == QUOTE.mid {
            After {
                mid: "” ", ..after
            }
        } else {
            after
        };
        self.plain(&format!("“{title}"), after);
    }

    /// Names joined the IEEE way, then `, Ed.` or `, Eds.` if `role` says so.
    fn names(&mut self, names: &[String], role: &str, after: After) {
        let Some(first) = names.first() else {
            return;
        };
        if names.len() > MOST_NAMES {
            self.plain(&format!("{first} "), NOTHING);
            self.italic("et al.", NOTHING);
        } else {
            let joined = match names {
                [only] => only.clone(),
                [one, two] => format!("{one} and {two}"),
                [all @ .., last] => format!("{}, and {last}", all.join(", ")),
                [] => return,
            };
            self.plain(&joined, NOTHING);
        }
        if !role.is_empty() {
            self.plain(&format!(", {role}"), NOTHING);
        }
        self.after = after;
    }

    /// A field written as `label` and then its text, when it has any.
    fn labelled(&mut self, label: &str, text: &str, after: After) {
        if !text.is_empty() {
            self.plain(&format!("{label}{text}"), after);
        }
    }

    fn pages(&mut self, pages: &str, after: After) {
        if pages.is_empty() {
            return;
        }
        let label = if pages.contains(['-', '–', '—', ',']) {
            "pp. "
        } else {
            "p. "
        };
        self.labelled(label, pages, after);
    }

    /// A month and a year as `Nov. 1981`, or whichever there is.
    fn date(&mut self, month: &str, year: &str, after: After) {
        let date = [month, year]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !date.is_empty() {
            self.plain(&date, after);
        }
    }

    /// The address last, after a sentence ending, with no period after it.
    fn online(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        if !self.runs.is_empty() {
            self.after = self.after.ending_sentence();
        }
        self.plain("[Online]. Available: ", NOTHING);
        self.push(url, RunStyle::Url, NOTHING);
    }

    fn finish(mut self) -> Vec<Run> {
        self.punctuation(self.after.end);
        let mut merged: Vec<Run> = Vec::new();
        for run in self.runs {
            match merged.last_mut() {
                Some(last) if last.style == run.style && run.style != RunStyle::Url => {
                    last.text.push_str(&run.text);
                }
                _ => merged.push(run),
            }
        }
        merged
    }
}

fn article(reference: &Reference, b: &mut Builder) {
    b.names(&reference.authors, "", COMMA);
    b.quoted(&reference.title, QUOTE);
    if !reference.published_in.is_empty() {
        b.italic(&reference.published_in, COMMA);
    }
    b.labelled("vol. ", &reference.volume, COMMA);
    b.labelled("no. ", &reference.number, COMMA);
    b.pages(&reference.pages, COMMA);
    b.date(&reference.month, &reference.year, COMMA);
    b.labelled("doi: ", &reference.doi, COMMA);
    b.online(&reference.url);
}

fn conference(reference: &Reference, b: &mut Builder) {
    b.names(&reference.authors, "", COMMA);
    b.quoted(&reference.title, QUOTE);
    if !reference.published_in.is_empty() {
        b.plain("in ", NOTHING);
        b.italic(&reference.published_in, COMMA);
    }
    if !reference.location.is_empty() {
        b.plain(&reference.location, COMMA);
    }
    b.date(&reference.month, &reference.year, COMMA);
    b.pages(&reference.pages, COMMA);
    b.labelled("doi: ", &reference.doi, COMMA);
    b.online(&reference.url);
}

fn book(reference: &Reference, b: &mut Builder) {
    let editor_role = if reference.editors.len() > 1 {
        "Eds."
    } else {
        "Ed."
    };
    let in_chapter = !reference.chapter.is_empty();
    // Editors lead a book that has no author, unless a chapter is being cited from it.
    let editors_lead = reference.authors.is_empty() && !in_chapter;
    if editors_lead {
        b.names(&reference.editors, editor_role, COMMA);
    } else {
        b.names(&reference.authors, "", COMMA);
    }
    if in_chapter {
        b.quoted(&reference.chapter, QUOTE);
        if !reference.title.is_empty() {
            b.plain("in ", NOTHING);
        }
    }
    let editors_follow = !editors_lead && !reference.editors.is_empty();
    // Someone may have typed the abbreviation with the edition.
    let edition = (!reference.edition.is_empty()).then(|| {
        if reference.edition.to_lowercase().ends_with("ed.") {
            reference.edition.clone()
        } else {
            format!("{} ed.", reference.edition)
        }
    });
    // The title closes with a period inside its italics when nothing else comes before the
    // place and publisher.
    if edition.is_none() && !editors_follow {
        if !reference.title.is_empty() {
            let closed = if reference.title.ends_with('.') {
                reference.title.clone()
            } else {
                format!("{}.", reference.title)
            };
            b.italic(&closed, SPACE);
        }
    } else {
        if !reference.title.is_empty() {
            b.italic(&reference.title, COMMA);
        }
        if editors_follow {
            let last = edition.is_none();
            b.names(
                &reference.editors,
                editor_role,
                if last { PERIOD } else { COMMA },
            );
        }
        if let Some(edition) = edition {
            b.plain(&edition, PERIOD);
        }
    }
    let has_publisher = !reference.publisher.is_empty();
    if !reference.place.is_empty() {
        b.plain(&reference.place, if has_publisher { COLON } else { COMMA });
    }
    if has_publisher {
        b.plain(&reference.publisher, COMMA);
    }
    if !reference.year.is_empty() {
        b.plain(&reference.year, COMMA);
    }
    b.pages(&reference.pages, COMMA);
    b.online(&reference.url);
}

fn web(reference: &Reference, b: &mut Builder) {
    b.names(&reference.authors, "", PERIOD);
    b.quoted(&reference.title, SENTENCE_QUOTE);
    if !reference.site.is_empty() {
        b.plain(&reference.site, PERIOD);
    }
    b.labelled("Accessed: ", &reference.accessed, PERIOD);
    b.online(&reference.url);
}

/// `reference` in IEEE format as runs. A freeform one is its text, as one plain run.
pub fn entry(reference: &Reference) -> Vec<Run> {
    if reference.kind == Kind::Freeform {
        return vec![Run {
            text: reference.text.clone(),
            style: RunStyle::Plain,
        }];
    }
    let mut b = Builder::new();
    match reference.kind {
        Kind::Article => article(reference, &mut b),
        Kind::Conference => conference(reference, &mut b),
        Kind::Book => book(reference, &mut b),
        Kind::Web => web(reference, &mut b),
        Kind::Freeform => {}
    }
    b.finish()
}

/// The text of `entry`, with no styling.
pub fn plain(reference: &Reference) -> String {
    entry(reference).into_iter().map(|run| run.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    fn s(text: &str) -> String {
        text.to_string()
    }

    fn knuth() -> Reference {
        Reference {
            kind: Kind::Article,
            authors: names(&["D. E. Knuth", "M. F. Plass"]),
            title: s("Breaking paragraphs into lines"),
            published_in: s("Softw.: Pract. Exper."),
            volume: s("11"),
            number: s("11"),
            pages: s("1119–1184"),
            month: s("Nov."),
            year: s("1981"),
            ..Reference::default()
        }
    }

    fn zhao() -> Reference {
        Reference {
            kind: Kind::Conference,
            authors: names(&["J. Zhao", "G. Sun", "G. H. Loh", "Y. Xie"]),
            title: s("Energy-efficient GPU design with reconfigurable in-package graphics memory"),
            published_in: s("Proc. ACM/IEEE Int. Symp. Low Power Electron. Design (ISLPED)"),
            month: s("Jul."),
            year: s("2012"),
            pages: s("403–408"),
            doi: s("10.1145/2333660.2333752"),
            ..Reference::default()
        }
    }

    fn stein() -> Reference {
        Reference {
            kind: Kind::Book,
            authors: names(&["L. Stein"]),
            editors: names(&["J. S. Brake"]),
            chapter: s("Random patterns"),
            title: s("Computers and You"),
            place: s("New York, NY, USA"),
            publisher: s("Wiley"),
            year: s("1994"),
            pages: s("55–70"),
            ..Reference::default()
        }
    }

    fn smith() -> Reference {
        Reference {
            kind: Kind::Web,
            authors: names(&["J. Smith", "J. Doe"]),
            title: s("Obama inaugurated as President"),
            site: s("CNN.com"),
            accessed: s("Feb. 1, 2009"),
            url: s("http://www.cnn.com/POLITICS/01/21/obama_inaugurated/index.html"),
            ..Reference::default()
        }
    }

    #[test]
    fn an_article_is_written_in_ieee_format() {
        assert_eq!(
            plain(&knuth()),
            "D. E. Knuth and M. F. Plass, “Breaking paragraphs into lines,” Softw.: Pract. \
             Exper., vol. 11, no. 11, pp. 1119–1184, Nov. 1981."
        );
    }

    #[test]
    fn a_conference_paper_with_a_doi_is_written_in_ieee_format() {
        assert_eq!(
            plain(&zhao()),
            "J. Zhao, G. Sun, G. H. Loh, and Y. Xie, “Energy-efficient GPU design with \
             reconfigurable in-package graphics memory,” in Proc. ACM/IEEE Int. Symp. Low Power \
             Electron. Design (ISLPED), Jul. 2012, pp. 403–408, doi: 10.1145/2333660.2333752."
        );
    }

    #[test]
    fn a_conference_paper_with_a_location_is_written_in_ieee_format() {
        let sarkar = Reference {
            kind: Kind::Conference,
            authors: names(&["D. Sarkar", "K. V. Srivastava"]),
            title: s(
                "SRR-loaded antipodal Vivaldi antenna for UWB applications with tunable \
                      notch function",
            ),
            published_in: s("Proc. Int. Symp. Electromagn. Theory"),
            location: s("Hiroshima, Japan"),
            year: s("2013"),
            pages: s("466–469"),
            ..Reference::default()
        };
        assert_eq!(
            plain(&sarkar),
            "D. Sarkar and K. V. Srivastava, “SRR-loaded antipodal Vivaldi antenna for UWB \
             applications with tunable notch function,” in Proc. Int. Symp. Electromagn. Theory, \
             Hiroshima, Japan, 2013, pp. 466–469."
        );
    }

    #[test]
    fn a_book_without_an_edition_has_a_period_inside_its_title() {
        let klaus = Reference {
            kind: Kind::Book,
            authors: names(&["B. Klaus", "P. Horn"]),
            title: s("Robot Vision"),
            place: s("Cambridge, MA, USA"),
            publisher: s("MIT Press"),
            year: s("1986"),
            ..Reference::default()
        };
        assert_eq!(
            plain(&klaus),
            "B. Klaus and P. Horn, Robot Vision. Cambridge, MA, USA: MIT Press, 1986."
        );
        let runs = entry(&klaus);
        assert_eq!(
            runs[1],
            Run {
                text: s("Robot Vision."),
                style: RunStyle::Italic
            }
        );
    }

    #[test]
    fn a_book_with_an_edition_has_a_comma_after_its_title() {
        let mut book = Reference {
            kind: Kind::Book,
            authors: names(&["J. K. Author"]),
            title: s("Title of Book"),
            edition: s("2nd"),
            place: s("City, Country"),
            publisher: s("Publisher"),
            year: s("2020"),
            ..Reference::default()
        };
        assert_eq!(
            plain(&book),
            "J. K. Author, Title of Book, 2nd ed. City, Country: Publisher, 2020."
        );
        book.pages = s("5");
        assert!(plain(&book).ends_with("2020, p. 5."), "{}", plain(&book));
    }

    #[test]
    fn an_edition_typed_with_its_abbreviation_isnt_abbreviated_twice() {
        let mut book = Reference {
            kind: Kind::Book,
            title: s("Title of Book"),
            place: s("City"),
            publisher: s("Publisher"),
            ..Reference::default()
        };
        for typed in ["2nd ed.", "2nd Ed.", "2ND ED."] {
            book.edition = s(typed);
            assert_eq!(
                plain(&book),
                format!("Title of Book, {typed} City: Publisher."),
                "{typed}"
            );
        }
        book.edition = s("2nd");
        assert_eq!(plain(&book), "Title of Book, 2nd ed. City: Publisher.");
    }

    #[test]
    fn a_chapter_is_written_in_ieee_format() {
        assert_eq!(
            plain(&stein()),
            "L. Stein, “Random patterns,” in Computers and You, J. S. Brake, Ed. New York, NY, \
             USA: Wiley, 1994, pp. 55–70."
        );
        let mut chapter = stein();
        chapter.editors = names(&["A. One", "B. Two"]);
        chapter.edition = s("3rd");
        assert!(
            plain(&chapter).contains("A. One and B. Two, Eds., 3rd ed. New York"),
            "{}",
            plain(&chapter)
        );
    }

    #[test]
    fn a_book_with_only_editors_leads_with_them() {
        let book = Reference {
            kind: Kind::Book,
            editors: names(&["X. Editor"]),
            title: s("Title"),
            ..Reference::default()
        };
        assert_eq!(plain(&book), "X. Editor, Ed., Title.");
        let runs = entry(&book);
        assert_eq!(runs.last().unwrap().style, RunStyle::Italic);
        assert_eq!(runs.last().unwrap().text, "Title.");
    }

    #[test]
    fn a_web_page_is_written_in_ieee_format() {
        assert_eq!(
            plain(&smith()),
            "J. Smith and J. Doe. “Obama inaugurated as President.” CNN.com. Accessed: Feb. 1, \
             2009. [Online]. Available: \
             http://www.cnn.com/POLITICS/01/21/obama_inaugurated/index.html"
        );
    }

    #[test]
    fn seven_names_become_the_first_and_et_al() {
        let mut article = knuth();
        article.authors = names(&[
            "A. One", "B. Two", "C. Three", "D. Four", "E. Five", "F. Six",
        ]);
        assert!(plain(&article)
            .starts_with("A. One, B. Two, C. Three, D. Four, E. Five, and F. Six, “"));
        article.authors.push(s("G. Seven"));
        assert!(
            plain(&article).starts_with("A. One et al., “Breaking"),
            "{}",
            plain(&article)
        );
        let runs = entry(&article);
        assert!(runs
            .iter()
            .any(|run| run.style == RunStyle::Italic && run.text == "et al."));
    }

    #[test]
    fn a_web_page_with_et_al_has_one_period() {
        let mut page = smith();
        page.authors = names(&["A", "B", "C", "D", "E", "F", "G"]);
        assert!(
            plain(&page).starts_with("A et al. “Obama"),
            "{}",
            plain(&page)
        );
    }

    #[test]
    fn a_title_alone_is_a_quoted_sentence() {
        let article = Reference {
            title: s("Title"),
            ..Reference::default()
        };
        assert_eq!(plain(&article), "“Title.”");
    }

    #[test]
    fn a_quoted_title_ending_in_a_period_has_no_comma_after_it() {
        let article = Reference {
            title: s("Birds, bees, etc."),
            published_in: s("Nature"),
            ..Reference::default()
        };
        assert_eq!(plain(&article), "“Birds, bees, etc.” Nature.");
        let alone = Reference {
            title: s("Birds, bees, etc."),
            ..Reference::default()
        };
        assert_eq!(plain(&alone), "“Birds, bees, etc.”");
    }

    #[test]
    fn leaving_a_field_out_leaves_no_stray_punctuation() {
        let mut full_book = stein();
        full_book.authors = names(&["L. Stein"]);
        full_book.edition = s("2nd");
        full_book.url = s("http://x.test/");
        let mut full_article = knuth();
        full_article.doi = s("10.1/x");
        full_article.url = s("http://x.test/");
        let mut full_conference = zhao();
        full_conference.location = s("Rome, Italy");
        full_conference.url = s("http://x.test/");
        let mut full_web = smith();
        full_web.authors = names(&["J. Smith"]);
        for full in [full_book, full_article, full_conference, full_web] {
            for field in full.kind.fields() {
                let mut blanked = full.clone();
                match *field {
                    "authors" => blanked.authors.clear(),
                    "editors" => blanked.editors.clear(),
                    "title" => blanked.title.clear(),
                    "chapter" => blanked.chapter.clear(),
                    "published_in" => blanked.published_in.clear(),
                    "location" => blanked.location.clear(),
                    "edition" => blanked.edition.clear(),
                    "place" => blanked.place.clear(),
                    "publisher" => blanked.publisher.clear(),
                    "site" => blanked.site.clear(),
                    "volume" => blanked.volume.clear(),
                    "number" => blanked.number.clear(),
                    "pages" => blanked.pages.clear(),
                    "month" => blanked.month.clear(),
                    "year" => blanked.year.clear(),
                    "doi" => blanked.doi.clear(),
                    "url" => blanked.url.clear(),
                    "accessed" => blanked.accessed.clear(),
                    other => panic!("{other}"),
                }
                let text = plain(&blanked);
                for bad in [", ,", "..", ",,", " ,", " .", ": :"] {
                    assert!(!text.contains(bad), "{field}: {text}");
                }
                assert!(
                    !text.ends_with(' ') && !text.starts_with(' '),
                    "{field}: {text}"
                );
                assert!(
                    !text.starts_with(',') && !text.starts_with('.'),
                    "{field}: {text}"
                );
            }
        }
    }

    #[test]
    fn an_entry_ends_with_a_period_unless_it_ends_with_a_url() {
        assert!(plain(&knuth()).ends_with('.'));
        assert!(plain(&stein()).ends_with('.'));
        assert!(!plain(&smith()).ends_with('.'));
        let mut article = knuth();
        article.url = s("http://x.test/a");
        assert!(plain(&article).ends_with("http://x.test/a"));
    }

    #[test]
    fn a_doi_comes_before_a_url_and_the_url_is_last() {
        let mut article = knuth();
        article.doi = s("10.1/x");
        article.url = s("http://x.test/a");
        assert_eq!(
            plain(&article),
            "D. E. Knuth and M. F. Plass, “Breaking paragraphs into lines,” Softw.: Pract. \
             Exper., vol. 11, no. 11, pp. 1119–1184, Nov. 1981, doi: 10.1/x. [Online]. \
             Available: http://x.test/a"
        );
        let runs = entry(&article);
        let last = runs.last().unwrap();
        assert_eq!(last.style, RunStyle::Url);
        assert_eq!(last.text, "http://x.test/a");
        assert_eq!(runs.iter().filter(|r| r.style == RunStyle::Url).count(), 1);
    }

    #[test]
    fn a_title_with_a_url_and_nothing_else_closes_its_quote() {
        let article = Reference {
            title: s("T"),
            url: s("http://x.test/"),
            ..Reference::default()
        };
        assert_eq!(plain(&article), "“T.” [Online]. Available: http://x.test/");
    }

    #[test]
    fn a_freeform_entry_is_its_text_exactly() {
        let freeform = Reference {
            kind: Kind::Freeform,
            text: s("As *typed*, with <marks> & no period"),
            ..Reference::default()
        };
        assert_eq!(
            entry(&freeform),
            vec![Run {
                text: s("As *typed*, with <marks> & no period"),
                style: RunStyle::Plain
            }]
        );
    }

    #[test]
    fn field_text_is_kept_as_typed() {
        let article = Reference {
            authors: names(&["é <b> & Q"]),
            title: s("lower *case* [x]"),
            ..Reference::default()
        };
        assert_eq!(plain(&article), "é <b> & Q, “lower *case* [x].”");
    }

    #[test]
    fn a_web_page_with_a_title_and_a_url_keeps_its_closing_quote() {
        let page = Reference {
            kind: Kind::Web,
            title: s("Page"),
            url: s("https://a.b/c"),
            ..Reference::default()
        };
        assert_eq!(plain(&page), "“Page.” [Online]. Available: https://a.b/c");
    }
}
