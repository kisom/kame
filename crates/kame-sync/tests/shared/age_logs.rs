//! Logs as an older build wrote them, for tests here and in `kame-notebook`.

use std::path::Path;

/// Rewrites a document's logs as a build from before `Metadata::publish` wrote them. This build still
/// parses each line, and works out another id for it, so that it no longer checks. Says what `state`
/// was called then.
pub fn age_logs(folder: &Path, state: &str) -> String {
    let mut renamed: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let mut aged = String::new();
        for line in text.lines() {
            let mut line = line.replace(",\"publish\":false", "");
            for (old, new) in &renamed {
                line = line.replace(old.as_str(), new.as_str());
            }
            let start = line.find("\"revision\":").unwrap() + "\"revision\":".len();
            let end = line.rfind(",\"blocks\":[").unwrap();
            let id = kame_sync::digest(&line.as_bytes()[start..end]);
            let old = line["{\"id\":\"".len()..]
                .split('"')
                .next()
                .unwrap()
                .to_string();
            line = line.replacen(&old, &id, 1);
            renamed.push((old, id));
            aged.push_str(&line);
            aged.push('\n');
        }
        std::fs::write(entry.path(), aged).unwrap();
    }
    renamed
        .into_iter()
        .find(|(old, _)| old == state)
        .map(|(_, new)| new)
        .unwrap()
}
