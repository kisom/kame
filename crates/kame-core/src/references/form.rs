//! What the windows for a new reference and for editing one hold while they're up: the text of
//! each field as typed, the key, and what would be saved. Drawing it is the app's.

use super::{suggest_key, valid_key, Kind, Reference, References, NAME_SUFFIXES};

/// What's wrong with a key that isn't one.
pub const NOT_A_KEY: &str = "A key is letters, digits, - and _, starting with a letter.";

/// The form's two tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Structured,
    Freeform,
}

/// What the new reference and edit windows hold while they're up. `reference` keeps everything
/// typed, so that switching kinds and tabs loses nothing, and `built` takes what will be saved.
pub struct ReferenceForm {
    pub tab: Tab,
    pub key: String,
    pub reference: Reference,
    /// The names as typed, as `names` reads them.
    pub authors_text: String,
    pub editors_text: String,
    /// Whether the key is the person's, and not to be suggested over.
    pub key_edited: bool,
    /// The key this is editing, if it isn't a new one.
    editing: Option<String>,
}

impl ReferenceForm {
    /// A form for a new reference, with `key` if there's one already.
    pub fn new(key: Option<String>) -> ReferenceForm {
        ReferenceForm {
            tab: Tab::default(),
            key_edited: key.is_some(),
            key: key.unwrap_or_default(),
            reference: Reference::default(),
            authors_text: String::new(),
            editors_text: String::new(),
            editing: None,
        }
    }

    /// A form for changing `reference`. Its key stays: renaming is in the References… window, as
    /// it rewrites documents.
    pub fn editing(key: &str, reference: &Reference) -> ReferenceForm {
        let freeform = reference.kind == Kind::Freeform;
        ReferenceForm {
            tab: if freeform {
                Tab::Freeform
            } else {
                Tab::Structured
            },
            key: key.to_string(),
            reference: Reference {
                // The kind the Structured tab starts on, should the person go there.
                kind: if freeform {
                    Kind::default()
                } else {
                    reference.kind
                },
                ..reference.clone()
            },
            authors_text: names_text(&reference.authors),
            editors_text: names_text(&reference.editors),
            key_edited: true,
            editing: Some(key.to_string()),
        }
    }

    /// Whether this is a new reference, rather than one being edited.
    pub fn is_new(&self) -> bool {
        self.editing.is_none()
    }

    /// The reference to save: only the fields its kind has, and freeform text exactly as typed.
    pub fn built(&self) -> Reference {
        let kind = match self.tab {
            Tab::Freeform => Kind::Freeform,
            Tab::Structured => self.reference.kind,
        };
        let mut typed = self.reference.clone();
        let mut built = Reference {
            kind,
            ..Reference::default()
        };
        for name in kind.fields() {
            match *name {
                "authors" => built.authors = names_from(&self.authors_text, &typed.authors),
                "editors" => built.editors = names_from(&self.editors_text, &typed.editors),
                "text" => built.text.clone_from(&typed.text),
                name => {
                    if let (Some(to), Some(from)) =
                        (text_field(&mut built, name), text_field(&mut typed, name))
                    {
                        *to = from.trim().to_string();
                    }
                }
            }
        }
        built
    }

    /// What's wrong with the key, if anything.
    pub fn key_problem(&self, references: &References) -> Option<String> {
        if self.key.is_empty() {
            return Some("It needs a key.".to_string());
        }
        if !valid_key(&self.key) {
            return Some(NOT_A_KEY.to_string());
        }
        if self.editing.as_deref() == Some(&self.key) {
            return None;
        }
        // Keys differing only in case can't both be saved, for the systems that ignore it.
        references
            .keys()
            .find(|key| key.eq_ignore_ascii_case(&self.key))
            .map(|key| format!("{key} is already a reference."))
    }

    /// What stops this being saved, if anything.
    pub fn problem(&self, references: &References) -> Option<String> {
        self.key_problem(references).or_else(|| {
            let built = self.built();
            if built.kind == Kind::Freeform && built.text.trim().is_empty() {
                return Some("text is missing".to_string());
            }
            built.check().err()
        })
    }

    /// Whether nothing stops this being saved.
    pub fn can_save(&self, references: &References) -> bool {
        self.problem(references).is_none()
    }

    /// Fills in a key from the fields, unless the key is the person's.
    pub fn suggest(&mut self, references: &References) {
        if self.key_edited {
            return;
        }
        let named = Reference {
            authors: names(&self.authors_text),
            ..self.reference.clone()
        };
        self.key = suggest_key(&named, references).unwrap_or_default();
    }
}

/// Names, each as spelled: split on semicolons if there are any, so that a name can hold a comma,
/// and otherwise on commas, keeping a suffix such as `Jr.` with the name before it.
fn names(text: &str) -> Vec<String> {
    let separator = if text.contains(';') { ';' } else { ',' };
    let mut found: Vec<String> = Vec::new();
    for name in text.split(separator).map(str::trim) {
        if name.is_empty() {
            continue;
        }
        match found.last_mut() {
            Some(before) if separator == ',' && NAME_SUFFIXES.contains(&name) => {
                before.push_str(", ");
                before.push_str(name);
            }
            _ => found.push(name.to_string()),
        }
    }
    found
}

/// The names in a box that was filled with `names_text(had)`: `had` as it was if the box is
/// unchanged, so that a name the box can't separate, holding a semicolon, isn't split by a save
/// that never touched it, and otherwise `names` of what's in the box.
fn names_from(text: &str, had: &[String]) -> Vec<String> {
    if text == names_text(had) {
        return had.to_vec();
    }
    names(text)
}

/// `names` as the form shows them, so that `names` reads them back the same. One name holding a
/// comma needs a semicolon even with nothing to separate, and so do names that commas would
/// join, as a name that is only a suffix would be.
fn names_text(names: &[String]) -> String {
    let joined = names.join(", ");
    if !names.iter().any(|name| name.contains(',')) && self::names(&joined) == names {
        return joined;
    }
    match names {
        [only] => format!("{only};"),
        names => names.join("; "),
    }
}

/// The field `name` of a reference, if it's one that holds a string.
pub fn text_field<'a>(reference: &'a mut Reference, name: &str) -> Option<&'a mut String> {
    Some(match name {
        "title" => &mut reference.title,
        "chapter" => &mut reference.chapter,
        "published_in" => &mut reference.published_in,
        "location" => &mut reference.location,
        "edition" => &mut reference.edition,
        "place" => &mut reference.place,
        "publisher" => &mut reference.publisher,
        "site" => &mut reference.site,
        "volume" => &mut reference.volume,
        "number" => &mut reference.number,
        "pages" => &mut reference.pages,
        "month" => &mut reference.month,
        "year" => &mut reference.year,
        "doi" => &mut reference.doi,
        "url" => &mut reference.url,
        "accessed" => &mut reference.accessed,
        "text" => &mut reference.text,
        _ => return None,
    })
}

/// What the form calls the field `name`.
pub fn field_label(name: &str) -> &'static str {
    match name {
        "authors" => "Authors",
        "editors" => "Editors",
        "title" => "Title",
        "chapter" => "Chapter",
        "published_in" => "Published in",
        "location" => "Location",
        "edition" => "Edition",
        "place" => "Place",
        "publisher" => "Publisher",
        "site" => "Site",
        "volume" => "Volume",
        "number" => "Number",
        "pages" => "Pages",
        "month" => "Month",
        "year" => "Year",
        "doi" => "DOI",
        "url" => "URL",
        "accessed" => "Accessed",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::plain;

    fn known(keys: &[&str]) -> References {
        let mut references = References::new();
        for key in keys {
            references.insert(
                key.to_string(),
                Reference {
                    title: "Breaking paragraphs into lines".to_string(),
                    ..Reference::default()
                },
            );
        }
        references
    }

    #[test]
    fn names_split_on_commas_and_keep_their_spelling() {
        let mut form = ReferenceForm::new(Some("k".to_string()));
        form.reference.kind = Kind::Book;
        form.reference.title = "T".to_string();
        form.authors_text = "D. E. Knuth,  M. F. Plass ,, van der Berg".to_string();
        form.editors_text = "Ünal Çelik".to_string();

        let built = form.built();

        assert_eq!(
            built.authors,
            ["D. E. Knuth", "M. F. Plass", "van der Berg"]
        );
        assert_eq!(built.editors, ["Ünal Çelik"]);
        // And back again, as an edit shows them.
        let again = ReferenceForm::editing("k", &built);
        assert_eq!(again.authors_text, "D. E. Knuth, M. F. Plass, van der Berg");
        assert_eq!(again.editors_text, "Ünal Çelik");
        assert_eq!(again.built(), built);
    }

    #[test]
    fn a_name_holding_a_comma_comes_back_whole_from_the_form() {
        let saved = |authors: &[&str], editors: &[&str]| Reference {
            kind: Kind::Book,
            title: "T".to_string(),
            authors: authors.iter().map(|name| name.to_string()).collect(),
            editors: editors.iter().map(|name| name.to_string()).collect(),
            ..Reference::default()
        };

        let two = saved(&["M. L. King, Jr.", "J. Doe"], &["Doe, J."]);
        let form = ReferenceForm::editing("k", &two);
        assert_eq!(form.authors_text, "M. L. King, Jr.; J. Doe");
        assert_eq!(form.built(), two);

        // On its own a name still needs a semicolon, or its comma would split it.
        let one = saved(&["M. L. King, Jr."], &[]);
        let form = ReferenceForm::editing("k", &one);
        assert_eq!(form.authors_text, "M. L. King, Jr.;");
        assert_eq!(form.built(), one);

        // Names with no comma look as they always did.
        let plain_names = saved(&["D. E. Knuth", "M. F. Plass"], &[]);
        let form = ReferenceForm::editing("k", &plain_names);
        assert_eq!(form.authors_text, "D. E. Knuth, M. F. Plass");
        assert_eq!(form.built(), plain_names);
    }

    #[test]
    fn a_name_holding_a_semicolon_is_kept_unless_its_box_is_changed() {
        let saved = Reference {
            kind: Kind::Book,
            title: "T".to_string(),
            authors: vec!["Doe; J.".to_string()],
            editors: vec!["A. Editor; Jr.".to_string(), "B. Other".to_string()],
            ..Reference::default()
        };
        let mut form = ReferenceForm::editing("k", &saved);
        assert_eq!(form.built(), saved);

        form.reference.title = "Retitled".to_string();
        assert_eq!(form.built().authors, saved.authors);
        assert_eq!(form.built().editors, saved.editors);

        // Typed in the box, the semicolon separates, as it does in any name typed there.
        form.authors_text = "Doe; J. Roe".to_string();
        assert_eq!(form.built().authors, ["Doe", "J. Roe"]);
    }

    #[test]
    fn a_suffix_after_a_comma_stays_with_its_name() {
        assert_eq!(names("M. L. King, Jr."), ["M. L. King, Jr."]);
        assert_eq!(names("A. Smith, B. Jones"), ["A. Smith", "B. Jones"]);
        assert_eq!(
            names("A. Smith, Jr., B. Jones, Sr"),
            ["A. Smith, Jr.", "B. Jones, Sr"]
        );
        assert_eq!(
            names("H. Ford, II, J. Doe,III, K. Roe , IV, L. Poe, Sr."),
            ["H. Ford, II", "J. Doe, III", "K. Roe, IV", "L. Poe, Sr."]
        );
        // With no name before it, it's a name of its own, and only the suffix itself counts.
        assert_eq!(names("Jr., A. Smith"), ["Jr.", "A. Smith"]);
        assert_eq!(names("A. Smith, Junior"), ["A. Smith", "Junior"]);

        let mut form = ReferenceForm::new(Some("k".to_string()));
        form.reference.kind = Kind::Book;
        form.reference.title = "T".to_string();
        form.authors_text = "M. L. King, Jr.".to_string();
        let built = form.built();
        assert_eq!(built.authors, ["M. L. King, Jr."]);
        assert_eq!(ReferenceForm::editing("k", &built).built(), built);

        // A name that is only a suffix is shown so that it doesn't join the one before.
        let odd = vec!["A. Smith".to_string(), "Jr.".to_string()];
        assert_eq!(names(&names_text(&odd)), odd);
    }

    #[test]
    fn a_semicolon_in_the_names_is_what_splits_them() {
        assert_eq!(names("King, M. L.; Doe, J."), ["King, M. L.", "Doe, J."]);
        assert_eq!(names("King, Jr.;"), ["King, Jr."]);
        assert_eq!(names(" ; ; A ;; "), ["A"]);
        assert_eq!(names("A, B"), ["A", "B"]);
    }

    #[test]
    fn a_suggested_key_follows_the_fields_until_its_edited() {
        let references = known(&["knuth1981"]);
        let mut form = ReferenceForm::new(None);
        assert!(!form.key_edited);
        assert_eq!(form.key, "");

        form.authors_text = "L. Lamport".to_string();
        form.reference.year = "1994".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "lamport1994");

        form.authors_text = "D. E. Knuth, M. F. Plass".to_string();
        form.reference.year = "1981".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "knuth1981a", "the plain one is taken");

        // With no letter left of the name, the person types one.
        form.authors_text = "李".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "");

        form.key = "mine".to_string();
        form.key_edited = true;
        form.authors_text = "L. Lamport".to_string();
        form.suggest(&references);
        assert_eq!(form.key, "mine");

        // A key that came with the form was already chosen.
        let mut given = ReferenceForm::new(Some("given".to_string()));
        given.authors_text = "L. Lamport".to_string();
        given.suggest(&references);
        assert_eq!(given.key, "given");
    }

    #[test]
    fn the_form_refuses_a_bad_key_a_taken_key_and_no_title() {
        let references = known(&["knuth1981"]);
        let mut form = ReferenceForm::new(Some("smith2020".to_string()));
        form.reference.title = "A title".to_string();
        assert_eq!(form.key_problem(&references), None);
        assert!(form.can_save(&references));

        for bad in ["", "1bad", "no way", "é"] {
            form.key = bad.to_string();
            assert!(form.key_problem(&references).is_some(), "{bad:?}");
            assert!(!form.can_save(&references), "{bad:?}");
        }
        for taken in ["knuth1981", "Knuth1981"] {
            form.key = taken.to_string();
            let problem = form.key_problem(&references).expect("a taken key");
            assert!(problem.contains("knuth1981"), "{problem}");
            assert!(!form.can_save(&references), "{taken:?}");
        }

        form.key = "smith2020".to_string();
        for missing in ["", "   "] {
            form.reference.title = missing.to_string();
            assert_eq!(form.key_problem(&references), None);
            assert!(!form.can_save(&references), "{missing:?}");
        }

        // A taken key is allowed when it's the key being edited.
        let editing = ReferenceForm::editing("knuth1981", references.get("knuth1981").unwrap());
        assert_eq!(editing.key_problem(&references), None);
        assert!(editing.can_save(&references));
    }

    #[test]
    fn freeform_saves_only_a_key_and_text_and_shows_them_as_typed() {
        let typed = "A letter,  1 May 1984.\nSecond line: \"quoted\" ";
        let mut form = ReferenceForm::new(Some("letter".to_string()));
        form.authors_text = "D. E. Knuth".to_string();
        form.reference.title = "Left on the other tab".to_string();
        form.reference.text = typed.to_string();
        form.tab = Tab::Freeform;

        assert!(form.can_save(&References::new()));
        assert_eq!(
            form.built(),
            Reference {
                kind: Kind::Freeform,
                text: typed.to_string(),
                ..Reference::default()
            }
        );
        assert_eq!(plain(&form.built()), typed);

        // What was typed on the other tab is still there.
        form.tab = Tab::Structured;
        assert_eq!(form.built().title, "Left on the other tab");
        assert_eq!(form.built().text, "");

        // Text that's only spaces is no text.
        form.tab = Tab::Freeform;
        form.reference.text = " \n".to_string();
        assert!(!form.can_save(&References::new()));

        // A freeform reference opens on its tab, and keeps its text.
        let saved = Reference {
            kind: Kind::Freeform,
            text: typed.to_string(),
            ..Reference::default()
        };
        let editing = ReferenceForm::editing("letter", &saved);
        assert_eq!(editing.tab, Tab::Freeform);
        assert_eq!(editing.built(), saved);
    }

    #[test]
    fn what_the_form_builds_always_passes_check() {
        for kind in [
            Kind::Article,
            Kind::Conference,
            Kind::Book,
            Kind::Web,
            Kind::Freeform,
        ] {
            for tab in [Tab::Structured, Tab::Freeform] {
                let mut form = ReferenceForm::new(Some("k".to_string()));
                form.tab = tab;
                form.reference = Reference {
                    kind,
                    title: "T".to_string(),
                    chapter: "1".to_string(),
                    published_in: "J".to_string(),
                    location: "L".to_string(),
                    edition: "2".to_string(),
                    place: "P".to_string(),
                    publisher: "Q".to_string(),
                    site: "S".to_string(),
                    volume: "3".to_string(),
                    number: "4".to_string(),
                    pages: "5".to_string(),
                    month: "May".to_string(),
                    year: "1981".to_string(),
                    doi: "10.1/x".to_string(),
                    url: "https://example.com".to_string(),
                    accessed: "1 May 2020".to_string(),
                    text: "Words".to_string(),
                    ..Reference::default()
                };
                form.authors_text = "A. Author".to_string();
                form.editors_text = "E. Editor".to_string();
                assert_eq!(form.built().check(), Ok(()), "{kind:?} on {tab:?}");
                assert!(form.can_save(&References::new()), "{kind:?} on {tab:?}");
            }
        }
    }
}
