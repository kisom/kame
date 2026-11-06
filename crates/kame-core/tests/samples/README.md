# Samples from each release

What 1.x writes, every later 1.x reads. Each folder here is named for a release and holds files that release wrote,
with what reading them gave it. Every later build's tests read every folder:
`crates/kame-core/tests/written_by_releases.rs` the document, and `crates/kame-sync/tests/logs_from_releases.rs`
the logs.

**Nothing in a folder is ever changed** once it's committed, not even to make a test pass. A release's files are
what people have on their disks. If a later build means to read or export one differently, the test says so,
never the folder.

| File | What it is |
|---|---|
| `essay.kame` | A document holding every kind of block, style and annotation, whole-block and range anchors, links, images at relative paths, metadata with words and accepted marks, two snapshots, a history of every kind of step with two waiting to be redone, and where the writer was |
| `essay.md`, `essay.html` | Its Markdown and HTML export, byte for byte |
| `essay.json` | The rest of its reading: the document and metadata as read, the steps applied and waiting, where the writer was, each snapshot, and the Markdown with every step redone |
| `logged/` | A folder with logs turned on, holding the revision logs of another document, `logged/essay.kame`, from two machines, `talos` and `titan`, and no database, as on a machine that hasn't opened it. They hold a merge, and end in two heads that change the same sentence |
| `logged.json` | Their reading: each head's document and metadata, and the merge of the two, with its id |

A release adds its folder with the generator, from the release's commit, before it's tagged:

```sh
cargo run -p kame-sync --example write_samples
```

It writes the folder for the workspace's version, and refuses one that's already here. The release workflow's
`check` job fails a tag whose version has no folder here.
