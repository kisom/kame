//! Finding likely mistakes in a document's text, offline. Nothing here changes a word or draws
//! anything. See `docs/superpowers/specs/2026-11-03-spelling-grammar-design.md`.

use harper_core::linting::{LintGroup, LintKind, Suggestion};
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document};
use kame_core::model::{bare_link_ranges, AcceptedMark, Block, BlockKind, SpanStyle};
use kame_core::references::citation_ranges;
use std::sync::mpsc::{channel, Receiver, Sender};

/// Rules that mark more that's right than wrong in the writing this was tried on.
const QUIET_RULES: [&str; 1] = ["FindOut"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindingKind {
    Spelling,
    Grammar,
}

/// A likely mistake in a block's text.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// Byte offsets into the text.
    pub start: usize,
    pub end: usize,
    pub kind: FindingKind,
    /// Which of the checker's rules it came from, to turn it off or accept it.
    pub rule: String,
    pub message: String,
    /// What could replace the marked text. Empty means removing it.
    pub suggestions: Vec<String>,
}

/// What the writer has said not to mark.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exceptions<'a> {
    /// The writer's own word list and the document's.
    pub words: &'a [String],
    pub document_words: &'a [String],
    pub accepted: &'a [AcceptedMark],
    pub rules_off: &'a [String],
}

impl Exceptions<'_> {
    /// Whether `finding`, in `text`, should be marked.
    pub fn allows(&self, finding: &Finding, text: &str) -> bool {
        let Some(shown) = text.get(finding.start..finding.end) else {
            return false;
        };
        let listed = |words: &[String]| words.iter().any(|word| word.eq_ignore_ascii_case(shown));
        !listed(self.words)
            && !listed(self.document_words)
            && !self.rules_off.contains(&finding.rule)
            && !self
                .accepted
                .iter()
                .any(|mark| mark.rule == finding.rule && mark.text == shown)
    }
}

/// Kinds of finding that are a matter of taste rather than a mistake.
const QUIET_KINDS: [LintKind; 4] = [
    LintKind::Readability,
    LintKind::Style,
    LintKind::Enhancement,
    LintKind::Formatting,
];
/// The most suggestions a mark offers.
const MOST_SUGGESTIONS: usize = 5;

/// Holds the dictionary and rules, which take a moment to load.
pub struct Checker {
    linter: LintGroup,
}

impl Checker {
    pub fn new() -> Checker {
        Checker {
            linter: LintGroup::new_curated(FstDictionary::curated(), Dialect::American),
        }
    }

    /// Everything to mark in `text`, in order, with no two at the same place.
    pub fn check(&mut self, text: &str) -> Vec<Finding> {
        let document = Document::new_plain_english_curated(text);
        // The checker counts characters, and everything in kame counts bytes.
        let mut bytes: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
        bytes.push(text.len());
        let chars: Vec<char> = text.chars().collect();
        let mut findings: Vec<Finding> = Vec::new();
        for (rule, lints) in self.linter.organized_lints(&document) {
            if QUIET_RULES.contains(&rule.as_str()) {
                continue;
            }
            for lint in lints {
                if QUIET_KINDS.contains(&lint.lint_kind) {
                    continue;
                }
                let (from, to) = (lint.span.start, lint.span.end.min(chars.len()));
                let (Some(start), Some(end)) = (bytes.get(from), bytes.get(to)) else {
                    continue;
                };
                let shown: String = chars[from..to].iter().collect();
                findings.push(Finding {
                    start: *start,
                    end: *end,
                    kind: match lint.lint_kind {
                        LintKind::Spelling | LintKind::Typo => FindingKind::Spelling,
                        _ => FindingKind::Grammar,
                    },
                    rule: rule.clone(),
                    message: lint.message,
                    suggestions: lint
                        .suggestions
                        .iter()
                        .map(|suggestion| replacement(suggestion, &shown))
                        .collect(),
                });
            }
        }
        // Spelling first, so that it names a place two rules share.
        findings.sort_by_key(|f| {
            (
                f.start,
                f.end,
                f.kind != FindingKind::Spelling,
                f.rule.clone(),
            )
        });
        let mut merged: Vec<Finding> = Vec::new();
        for finding in findings {
            match merged.last_mut() {
                Some(last) if (last.start, last.end) == (finding.start, finding.end) => {
                    // A rule that knows the exact answer says more than a guess at a spelling.
                    if last.suggestions.len() != 1 && finding.suggestions.len() == 1 {
                        last.message = finding.message.clone();
                        last.rule = finding.rule.clone();
                        last.suggestions.clear();
                    }
                    for suggestion in finding.suggestions {
                        if !last.suggestions.contains(&suggestion) {
                            last.suggestions.push(suggestion);
                        }
                    }
                }
                _ => merged.push(finding),
            }
        }
        for finding in &mut merged {
            finding.suggestions.truncate(MOST_SUGGESTIONS);
        }
        merged
    }

    /// Everything to mark in a block, which is nothing in the kinds that aren't the writer's
    /// prose, or in its code, links and citations.
    pub fn check_block(&mut self, block: &Block) -> Vec<Finding> {
        if !matches!(
            block.kind,
            BlockKind::Paragraph | BlockKind::Heading { .. } | BlockKind::ListItem { .. }
        ) {
            return Vec::new();
        }
        let mut skipped: Vec<(usize, usize)> = block
            .spans
            .iter()
            .filter(|span| {
                matches!(
                    span.style,
                    SpanStyle::Code | SpanStyle::Link { .. } | SpanStyle::DocumentLink { .. }
                )
            })
            .map(|span| (span.start, span.end))
            .collect();
        skipped.extend(bare_link_ranges(block));
        // Whether or not its key has a reference, since this has no list to ask.
        skipped.extend(citation_ranges(&block.text));
        self.check(&block.text)
            .into_iter()
            .filter(|f| {
                !skipped
                    .iter()
                    .any(|(from, to)| f.start < *to && *from < f.end)
            })
            .collect()
    }
}

impl Default for Checker {
    fn default() -> Self {
        Checker::new()
    }
}

/// A checker on its own thread, since it takes a moment to load and the page can't wait for it.
pub struct Background {
    asks: Sender<Block>,
    answers: Receiver<(Block, Vec<Finding>)>,
}

impl Background {
    /// Starts it. `wake` is called after each answer, to tell the page there's something new.
    pub fn new(wake: impl Fn() + Send + 'static) -> Background {
        let (asks, asked) = channel::<Block>();
        let (answered, answers) = channel();
        std::thread::spawn(move || {
            let mut checker = Checker::new();
            while let Ok(block) = asked.recv() {
                let findings = checker.check_block(&block);
                if answered.send((block, findings)).is_err() {
                    break;
                }
                wake();
            }
        });
        Background { asks, answers }
    }

    pub fn ask(&self, block: Block) {
        // If the thread has gone, there's simply nothing marked.
        let _ = self.asks.send(block);
    }

    /// The answers so far, without waiting.
    pub fn answers(&self) -> Vec<(Block, Vec<Finding>)> {
        self.answers.try_iter().collect()
    }
}

/// What a suggestion would put in place of the marked text.
fn replacement(suggestion: &Suggestion, shown: &str) -> String {
    match suggestion {
        Suggestion::ReplaceWith(chars) => chars.iter().collect(),
        Suggestion::InsertAfter(chars) => {
            let after: String = chars.iter().collect();
            format!("{shown}{after}")
        }
        Suggestion::Remove => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kame_core::model::Span;
    use std::sync::{Mutex, OnceLock};

    /// One checker for all the tests, since loading it takes a moment.
    fn check(text: &str) -> Vec<Finding> {
        static CHECKER: OnceLock<Mutex<Checker>> = OnceLock::new();
        CHECKER
            .get_or_init(|| Mutex::new(Checker::new()))
            .lock()
            .unwrap()
            .check(text)
    }

    fn check_block(block: &Block) -> Vec<Finding> {
        Checker::new().check_block(block)
    }

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            id: uuid::Uuid::new_v4(),
            kind,
            text: text.to_string(),
            spans: Vec::new(),
        }
    }

    fn marked<'a>(text: &'a str, finding: &Finding) -> &'a str {
        &text[finding.start..finding.end]
    }

    #[test]
    fn the_background_checker_answers_and_wakes_the_page() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let woken = Arc::new(AtomicUsize::new(0));
        let counted = woken.clone();
        let background = Background::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        assert!(background.answers().is_empty());
        let asked = block(BlockKind::Paragraph, "An indispensible tool.");
        background.ask(asked.clone());
        let began = std::time::Instant::now();
        let answers = loop {
            let answers = background.answers();
            if !answers.is_empty() {
                break answers;
            }
            assert!(began.elapsed().as_secs() < 60, "no answer");
            std::thread::yield_now();
        };
        assert_eq!(answers[0].0, asked);
        assert_eq!(answers[0].1.len(), 1);
        assert_eq!(woken.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_misspelling_is_marked_with_a_suggestion() {
        let text = "This is an indispensible tool.";
        let findings = check(text);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(marked(text, &findings[0]), "indispensible");
        assert_eq!(findings[0].kind, FindingKind::Spelling);
        assert!(
            findings[0].suggestions.iter().any(|s| s == "indispensable"),
            "{:?}",
            findings[0].suggestions
        );
    }

    #[test]
    fn clean_text_is_not_marked() {
        assert_eq!(
            check("The quick brown fox jumps over the lazy dog."),
            vec![]
        );
    }

    #[test]
    fn offsets_are_bytes_after_multibyte_text() {
        let text = "The caf\u{e9} \u{2014} na\u{ef}ve \u{2014} has an indispensible menu.";
        let findings = check(text);
        let found = findings
            .iter()
            .find(|f| marked(text, f) == "indispensible")
            .expect("the misspelling, at its byte offset");
        assert!(text.is_char_boundary(found.start));
    }

    #[test]
    fn a_product_name_is_corrected() {
        let text = "I pushed it to my Github account.";
        let findings = check(text);
        assert!(
            findings
                .iter()
                .any(|f| marked(text, f) == "Github" && f.suggestions.iter().any(|s| s == "GitHub")),
            "{findings:?}"
        );
    }

    #[test]
    fn two_rules_at_one_place_make_one_mark() {
        let findings = check("I pushed it to my Github account.");
        assert_eq!(findings.len(), 1, "{findings:?}");
    }

    #[test]
    fn a_long_sentence_is_not_a_mistake() {
        let text = "This sentence goes on and on, as sentences sometimes do when a writer has a \
                    great many things to say and no particular wish to stop, because the thought \
                    keeps unfolding, and each clause suggests another, and the reader, who has come \
                    this far, might as well see where it ends, which is here, more or less, today.";
        assert_eq!(check(text), vec![]);
    }

    #[test]
    fn findings_come_in_order() {
        let findings = check("Thiss has twoo misspellings.");
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings[0].start < findings[1].start);
    }

    #[test]
    fn only_the_writers_prose_is_checked() {
        let wrong = "An indispensible tool.";
        for kind in [
            BlockKind::Paragraph,
            BlockKind::Heading {
                level: 1,
                centered: false,
            },
            BlockKind::ListItem {
                depth: 0,
                ordered: false,
            },
        ] {
            assert_eq!(
                check_block(&block(kind.clone(), wrong)).len(),
                1,
                "{kind:?}"
            );
        }
        for kind in [
            BlockKind::Code { language: None },
            BlockKind::Prose,
            BlockKind::BlockQuote,
        ] {
            assert_eq!(check_block(&block(kind.clone(), wrong)), vec![], "{kind:?}");
        }
    }

    #[test]
    fn code_and_links_are_not_marked() {
        let text = "See indispensible for more, and https://indispensible.example/ too.";
        for style in [
            SpanStyle::Code,
            SpanStyle::Link {
                url: "https://a.b".to_string(),
            },
            SpanStyle::DocumentLink {
                target: "other".to_string(),
            },
        ] {
            let mut linked = block(BlockKind::Paragraph, text);
            linked.spans.push(Span {
                start: 4,
                end: 17,
                style,
            });
            let findings = check_block(&linked);
            assert!(
                findings.iter().all(|f| marked(text, f) != "indispensible"),
                "{findings:?}"
            );
        }
    }

    fn finding(text: &str, word: &str, rule: &str) -> Finding {
        let start = text.find(word).unwrap();
        Finding {
            start,
            end: start + word.len(),
            kind: FindingKind::Spelling,
            rule: rule.to_string(),
            message: String::new(),
            suggestions: Vec::new(),
        }
    }

    #[test]
    fn a_word_in_either_list_is_not_marked() {
        let text = "Written in kame today.";
        let found = finding(text, "kame", "SpellCheck");
        assert!(Exceptions::default().allows(&found, text));
        let mine = vec!["kame".to_string()];
        let in_mine = Exceptions {
            words: &mine,
            ..Exceptions::default()
        };
        assert!(!in_mine.allows(&found, text));
        let in_document = Exceptions {
            document_words: &mine,
            ..Exceptions::default()
        };
        assert!(!in_document.allows(&found, text));
    }

    #[test]
    fn a_word_list_ignores_case() {
        let text = "Kame is here.";
        let found = finding(text, "Kame", "SpellCheck");
        let mine = vec!["kame".to_string()];
        let exceptions = Exceptions {
            words: &mine,
            ..Exceptions::default()
        };
        assert!(!exceptions.allows(&found, text));
    }

    #[test]
    fn accepting_a_mark_is_for_that_rule_and_that_text() {
        let text = "A key point here and a key point there, and another thing.";
        let found = finding(text, "key point", "Compounds");
        let accepted = vec![AcceptedMark {
            rule: "Compounds".to_string(),
            text: "key point".to_string(),
        }];
        let exceptions = Exceptions {
            accepted: &accepted,
            ..Exceptions::default()
        };
        assert!(!exceptions.allows(&found, text));
        assert!(exceptions.allows(&finding(text, "another", "Compounds"), text));
        assert!(exceptions.allows(&finding(text, "key point", "Other"), text));
    }

    #[test]
    fn a_rule_turned_off_marks_nothing() {
        let text = "Some words.";
        let off = vec!["Compounds".to_string()];
        let exceptions = Exceptions {
            rules_off: &off,
            ..Exceptions::default()
        };
        assert!(!exceptions.allows(&finding(text, "words", "Compounds"), text));
        assert!(exceptions.allows(&finding(text, "words", "Other"), text));
    }

    #[test]
    fn a_citation_isnt_a_misspelling() {
        // The checker leaves most of these alone itself, taking them for Markdown's links, but
        // reads on past an underscore.
        for text in [
            "As shown in [zzqxj2020] already.",
            "As shown in [foo_zzqxj] already.",
        ] {
            assert_eq!(check_block(&block(BlockKind::Paragraph, text)), Vec::new());
        }

        let bare = block(BlockKind::Paragraph, "As shown in zzqxj already.");
        let found = check_block(&bare);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(&bare.text[found[0].start..found[0].end], "zzqxj");
        assert_eq!(found[0].kind, FindingKind::Spelling);
    }
}
