# Spelling and grammar check — design

Date: 2026-11-03
Status: accepted 2026-11-03, with option A from section 4. The trial has run, and Harper missed the mark it was
set for grammar, so it checks spelling, names and compounds. Section 8 lists the decisions made on your behalf.

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

**Recommendation:** after the trial below, Harper as a spell checker with a few of its rules, and an LLM for
grammar when asked. Harper is Apache-2.0.

## 4. Stage 0: the trial

`harper-core` 3.0.0 ran outside the repo over copies of both essays, and over 31 sentences holding mistakes from
the review.

| Kind | Marked | Which it found |
|---|---|---|
| Misspelled words | 3 of 3 | all of them |
| Names | 2 of 3 | wrong capitals in product names, not a misspelled surname |
| Compounds and hyphens | 2 of 8 | only words it knows as closed compounds, and none of the hyphenated modifiers |
| A wrong or extra word | 1 of 8 | none of the plain ones, like a stray verb or a doubled auxiliary |
| A missing word | 1 of 9 | one, with an unhelpful message |
| **All** | **9 of 31** | |

The bar was most spelling and hyphen mistakes and some wrong-word ones. **It clears spelling and fails the rest.**
The test sentences came from a reader's review, so that review's score against them means nothing, but it's
plainly the better grammar checker for now.

It also found things the review didn't: a fresh typo made since the review, a double space, a misspelled compound
and a wrongly capitalized product name.

**False alarms are low once you see what they are.** Without its readability rule, which complains about long
sentences and should simply be off, *AgainstBrainrot* has about 7 false alarms in about 2,770 words, or 2.5 per
1,000, beating the bar of 5. A few rules cause most of them, and they can go. Unknown words are the larger group:
about 15 distinct ones, such as a product name or this editor's own, each marked until it's added to a word list
once. Some words get two marks, so marks at one place should be merged.

**Speed is fine, with one catch.** After 330 ms to start and about 800 ms for the first check, a block takes a few
milliseconds. So it has to start on a background thread.

**It's heavy.** It adds 99 crates to kame's build, and about 17.5 MB to a release binary, against kame's 44 MB.

**The decision for you:**

- **A: Harper for spelling, names and compounds,** with its readability and formatting rules off. It does all this
  design describes, with grammar left to the second layer. The cost is the weight above.
- **B: a small spell checker instead,** such as `spellbook` with a Hunspell dictionary, if one proves good enough.
  It would be far lighter, and it would lose the product names and compounds, which is 4 of Harper's 9. It's
  untried, so it would need its own short trial.
- **C: stop at the review pass,** with no marks at all, since rules caught under a third of what a reader did.

I'd take **A**. It's a heavy dependency, but it's one crate from a maintained project, it's already written, and
marks you can act on are what's missing. Spelling alone would have caught the kind of slip that survives
rereading.

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
footnotes are checked as drawn on the page, with the same right-click menu, and F7 stays in the text. Notes aren't
checked, since those are the writer's own thinking. While a popover is open, marks don't appear in its field.

**When.** On a background thread, about half a second after typing stops, and only for changed blocks, which are
cached by their text. The page never waits for it. The word under the caret isn't marked while you're still
typing it.

**How it looks.** A thin wavy underline, drawn with `range_rects` so that it follows hyphen breaks and sideways
scrolling, in one colour for spelling and a quieter one for grammar. In hide-chrome mode it stays, since it's
on the page, and the View menu turns it off entirely.

**Acting on a mark.** Right-clicking one shows its message and these choices. A key steps to the next mark.

| Choice | What it does | Where it's kept |
|---|---|---|
| A suggestion | changes the text, through an edit command, so it's one invertible `Op` and one undo | the document's text |
| **Accept this** | says the checker is wrong here: that rule stops marking this text in this document | the document |
| **Add to this document's words** | for a name or term used only here | the document |
| **Add to my words** | for a word that's right everywhere | your config |
| **Turn off this rule** | for a rule you never want | your config |

**Two word lists.** One is global and lives in the config folder. One is local and lives in the document, so it
travels with it. A word in either isn't marked. The two lists only concern spelling; *Accept this* covers anything
else, and it's remembered as the rule and the marked text, so it survives edits elsewhere in the document, and comes
back if you change those words.

**Local choices are stored in `Metadata`.** It has the right behaviour already: changes to it aren't undo steps,
and restoring a snapshot leaves it alone. It gains `words` and `accepted`, both defaulting to empty, so older files
still open. Three things need care:

- **Merging.** Merging a conflict copy keeps one document's metadata today. These two fields must become the union of
  both, or words added on one machine would be lost.
- **Exports.** Neither field is published, so neither reaches HTML, Markdown or the site.
- **Old builds.** An older kame that rewrites the metadata would drop them, so update every machine together, as with
  the merge undo entry.

## 6. Layer 2: a review when asked

No code for now. Exporting a draft and asking an LLM to review it, as just happened, already works and catches what
rules can't. It's also the only layer that sends text out, so it should stay something you do on purpose. If it
becomes routine, a later design could show its findings as marks in the same way, and nothing here gets in the
way of that.

## 7. Stages after the trial

1. `kame-check` with Harper, tested on its own, including a small set of made-up sentences for each kind in
   section 2, so that the tests don't hold anyone's real writing.
2. Marks on the page, on the background thread, with the View setting.
3. The right-click menu, both word lists, accepting a mark and turning off rules, with the metadata fields and the
   merge change.
4. Margin notes and footnotes.

Each is its own commit, tried in the app before it lands.

## 8. Decisions made on your behalf

- **Which of the trial's outcomes to take is yours:** section 4 recommends A.
- **Accepting is per document,** by rule and marked text, since a mark that's wrong in one essay may be right in
  another. Turning a rule off is the global choice.
- **No automatic LLM layer,** to keep a firm rule that typing never sends text anywhere.
- **Off by default until you've tried it,** then on if it earns it.
- **Block quotes are skipped.**
- **American English,** since the essays use it. There's a stray British spelling or two in the draft, which
  this setting would catch. Say if you'd rather write British English.
- **The 5 false alarms per 1,000 words** was my guess at what's tolerable, about one per page. Your judgment
  from seeing marks on the page should overrule it.

## 9. What this doesn't show

- **The other options weren't run.** Their details in section 3 are from each project's own pages.
- **The 31 sentences** are a sample of the review's mistakes, shortened in places, and scored by whether a mark
  landed on the right spot, not whether its message helped.
- **Section 2 is one draft by one writer,** counted by one reader, so the shares are rough. Style calls were
  left out of the count.
- **The look of it.** Nothing here has been on the page, and that's what will settle whether the marks earn their place.
