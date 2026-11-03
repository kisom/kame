# Spelling and grammar check — design

Date: 2026-11-03
Status: draft for review. Nothing is built. The first step is a trial that hasn't run, and it decides
whether the rest is worth building. Section 8 lists the decisions made on your behalf.

## 1. Purpose

Rereading is the only thing catching mistakes in an essay today. The goal is marks on the page for likely
mistakes as you write, which never change a word unless you ask, and never send your writing anywhere
unless you ask.

## 2. What the mistakes look like

A review of the *AgainstBrainrot* first draft, about 2,800 words, found around 30 mistakes. They matter
more than any feature list, so this design is judged against them.

| Kind | Share | Example of the kind | What can catch it |
|---|---|---|---|
| Misspelled words and names | about 1 in 6 | `recieve`, a misspelled surname | a word list, except for names |
| Compounds and hyphens | about 1 in 4 | `a two minute change` | grammar rules |
| A wrong or extra word | about 1 in 4 | `the plan is has`, `what need doing` | grammar rules, some of them |
| A missing word | about 1 in 4 | `I went the store` | grammar rules seldom; a reader |
| Sentences that fell apart | about 1 in 10 | a fragment left by an edit | a reader |

Most of the mistakes are made of real words, so a spell checker alone would miss most of them. Some, like a
half-rewritten sentence, need something that reads for sense. No single tool does all of this, so the design
has two layers.

## 3. Options

| Option | Offline | Fit | Against it |
|---|---|---|---|
| **Harper** (`harper-core`) | yes, in process | Rust, built to embed, English, claims milliseconds per document | about 30 dependencies; what it catches is untested here |
| `nlprule` | yes, in process | Rust, LanguageTool's rules | last release 2021, LGPL, its rules date from LanguageTool 5.2 |
| LanguageTool server | yes, if run locally | the largest rule set | a Java server to run on every machine, which is a lot for one person's editor |
| A spell checker alone | yes | small and simple | misses most of section 2 |
| An LLM reading the draft | no | catches what rules can't, as the review above shows | sends the text out, is slow, and costs money, so it can't run as you type |

**Recommendation:** Harper for marks as you type, if it passes the trial, and an LLM only when asked. Harper's
claims come from its own README and are unverified here. Its license must also be checked before it's added,
since the page I read didn't state one.

## 4. Stage 0: the trial, before anything else

Run `harper-core` 3.0.0 outside the repo over copies of the two essays, printing what it marks, its time, and
its effect on the size of the app. Score it against the review's findings:

- **Found:** how many of the review's mistakes it marks, by kind.
- **False alarms:** how many marks aren't mistakes, per 1,000 words.

**Worth building if** it finds most of the spelling and hyphen mistakes and some of the wrong-word ones, with no
more than about 5 false alarms per 1,000 words after turning off noisy rules. A mark that's usually wrong gets
ignored, and then the real ones do too. **If it fails,** a spell checker alone is still worth a look, and
reading for sense stays with the second layer.

This trial downloads and runs a third-party crate, so it needs your permission.

## 5. Layer 1: marks as you type

**A crate, `kame-check`,** with no GUI, in keeping with the layering. It takes text and answers with findings:

```rust
pub struct Finding {
    pub start: usize, // byte offsets, like everything in kame
    pub end: usize,
    pub kind: FindingKind, // Spelling or Grammar
    pub message: String,
    pub suggestions: Vec<String>,
}
```

Harper's spans count characters, and kame's invariant is bytes, so this crate converts them, with tests on
multibyte text.

**What gets checked.** Paragraphs, headings and list items. Not code or prose blocks, and not inline code or
links. Not block quotes either, since those are someone else's words, and marking them is noise. Margin notes and
footnotes come later, because they're edited differently, even though the review found a misspelling in one.

**When.** On a background thread, about half a second after typing stops, and only for changed blocks, which are
cached by their text. The page never waits for it. The word under the caret isn't marked while you're still
typing it.

**How it looks.** A thin wavy underline, drawn with `range_rects` so that it follows hyphen breaks and sideways
scrolling, in one colour for spelling and a quieter one for grammar. In hide-chrome mode it stays, since it's
on the page, and the View menu turns it off entirely.

**Acting on a mark.** The right-click menu shows the message and suggestions. Taking a suggestion goes through an
edit command, so it's one invertible `Op` and one undo. *Add to dictionary* and *Ignore this rule* are the other
choices. A key steps to the next mark.

**What's kept.** Nothing in the document, so sync, exports and undo history are untouched. A personal word list
and the rules you've turned off go in the app's config folder. The word list should sync between machines,
so it belongs in the same folder as your writing if you prefer; see section 8.

## 6. Layer 2: a review when asked

No code for now. Exporting a draft and asking an LLM to review it, as just happened, already works and catches what
rules can't. It's also the only layer that sends text out, so it should stay something you do on purpose. If it
becomes routine, a later design could show its findings as marks in the same way, and nothing here gets in the
way of that.

## 7. Stages after the trial

1. `kame-check` with Harper, tested on its own, including a small set of made-up sentences for each kind in
   section 2, so that the tests don't hold anyone's real writing.
2. Marks on the page, on the background thread, with the View setting.
3. The right-click menu, the word list and turning off rules.
4. Margin notes and footnotes.

Each is its own commit, tried in the app before it lands.

## 8. Decisions made on your behalf

- **Harper first,** and only if the trial passes.
- **No automatic LLM layer,** to keep a firm rule that typing never sends text anywhere.
- **Off by default until you've tried it,** then on if it earns it.
- **Block quotes are skipped.**
- **American English,** since the essays use it. There's a stray British spelling or two in the draft, which
  this setting would catch. Say if you'd rather write British English.
- **The 5 false alarms per 1,000 words** is my guess at what's tolerable, about one per page. Your judgment
  from seeing the trial's real output should overrule it.
- **Where the word list lives** is open: the app's config folder is simple but doesn't sync, while beside your
  writing it would travel with Syncthing. I lean towards syncing it.

## 9. What this doesn't show

- **Nothing has been run.** The figures in section 3 are from each project's own pages.
- **Section 2 is one draft by one writer,** counted by one reader, so the shares are rough. Style calls were
  left out of the count.
- **Harper's effect on app size and start-up** is unknown. The dictionary may need loading on a background thread
  to keep start-up fast.
