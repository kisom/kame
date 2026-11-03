//! `site.yaml`.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;
use std::fmt;

use kame_core::links::resolve_open_path;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;

/// A map that refuses a repeated key. A plain map keeps the last of a duplicate, which
/// would quietly drop a document.
#[derive(Debug, Default)]
struct UniqueMap(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for UniqueMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;

        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueMap;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of slugs to files")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueMap, A::Error> {
                let mut found = BTreeMap::new();
                while let Some((slug, path)) = map.next_entry::<String, String>()? {
                    if found.insert(slug.clone(), path).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "the slug \"{slug}\" appears twice"
                        )));
                    }
                }
                Ok(UniqueMap(found))
            }
        }

        deserializer.deserialize_map(UniqueVisitor)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    site: RawSite,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSite {
    name: String,
    url: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    publish: UniqueMap,
    #[serde(default)]
    drafts: UniqueMap,
}

/// Lowercase letters, digits and hyphens, since a slug is a URL.
fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}
/// One document listed in `site.yaml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub slug: String,
    pub path: PathBuf,
}

/// A site's configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub name: String,
    pub url: String,
    pub description: String,
    pub author: String,
    pub publish: Vec<Entry>,
    pub drafts: Vec<Entry>,
}

impl Config {
    /// Reads `site.yaml` from `site_dir`.
    pub fn load(site_dir: &Path) -> Result<Config, Vec<String>> {
        let path = site_dir.join("site.yaml");
        let yaml = std::fs::read_to_string(&path)
            .map_err(|err| vec![format!("could not read {}: {err}", path.display())])?;
        Config::parse(&yaml, site_dir)
    }

    /// Reads a `site.yaml`'s text, with relative paths starting from `site_dir`. Every
    /// problem it can find is reported together.
    pub fn parse(yaml: &str, site_dir: &Path) -> Result<Config, Vec<String>> {
        let raw: Raw =
            yaml_serde::from_str(yaml).map_err(|err| vec![format!("site.yaml: {err}")])?;
        let site = raw.site;
        let base = site_dir.join("site.yaml");
        let entries = |map: UniqueMap| -> Vec<Entry> {
            map.0
                .into_iter()
                .map(|(slug, path)| Entry {
                    slug,
                    path: resolve_open_path(&path, &base),
                })
                .collect()
        };
        let publish = entries(site.publish);
        let drafts = entries(site.drafts);

        let mut errors = Vec::new();
        let mut slugs = BTreeMap::new();
        let mut files = BTreeMap::new();
        for entry in publish.iter().chain(&drafts) {
            if !valid_slug(&entry.slug) {
                errors.push(format!(
                    "the slug \"{}\" can only use lowercase letters, digits and hyphens",
                    entry.slug
                ));
            }
            if slugs.insert(entry.slug.clone(), ()).is_some() {
                errors.push(format!("the slug \"{}\" appears twice", entry.slug));
            }
            if let Some(other) = files.insert(entry.path.clone(), entry.slug.clone()) {
                errors.push(format!(
                    "{} is listed twice, as \"{other}\" and \"{}\"",
                    entry.path.display(),
                    entry.slug
                ));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Config {
            name: site.name,
            url: site.url.trim_end_matches('/').to_string(),
            description: site.description,
            author: site.author,
            publish,
            drafts,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "site:\n  name: Example\n  url: https://example.com\n  publish:\n    b-doc: /w/b.kame\n    a-doc: docs/a.kame\n  drafts:\n    c-doc: /w/c.kame\n";

    fn parse(yaml: &str) -> Result<Config, Vec<String>> {
        Config::parse(yaml, Path::new("/site"))
    }

    fn errors(yaml: &str) -> String {
        parse(yaml).unwrap_err().join("\n")
    }

    #[test]
    fn a_good_config_reads_with_entries_sorted_by_slug() {
        let config = parse(GOOD).unwrap();
        assert_eq!(config.name, "Example");
        let slugs: Vec<&str> = config.publish.iter().map(|e| e.slug.as_str()).collect();
        assert_eq!(slugs, ["a-doc", "b-doc"]);
        assert_eq!(config.drafts[0].slug, "c-doc");
    }

    #[test]
    fn a_relative_path_starts_from_the_site() {
        let config = parse(GOOD).unwrap();
        assert_eq!(config.publish[0].path, PathBuf::from("/site/docs/a.kame"));
        assert_eq!(config.publish[1].path, PathBuf::from("/w/b.kame"));
    }

    #[test]
    fn a_trailing_slash_on_the_url_is_dropped() {
        let yaml = "site:\n  name: E\n  url: https://example.com/\n  publish: {}\n";
        assert_eq!(parse(yaml).unwrap().url, "https://example.com");
    }

    #[test]
    fn an_empty_publish_list_is_fine() {
        let yaml = "site:\n  name: E\n  url: https://e.com\n  publish: {}\n";
        assert!(parse(yaml).unwrap().publish.is_empty());
    }

    #[test]
    fn a_duplicate_slug_is_an_error() {
        let yaml = "site:\n  name: E\n  url: https://e.com\n  publish:\n    a: /w/a.kame\n    a: /w/b.kame\n";
        assert!(errors(yaml).contains("\"a\""), "{}", errors(yaml));
    }

    #[test]
    fn a_slug_in_both_lists_is_an_error() {
        let yaml = "site:\n  name: E\n  url: https://e.com\n  publish:\n    a: /w/a.kame\n  drafts:\n    a: /w/b.kame\n";
        assert!(errors(yaml).contains("\"a\""));
    }

    #[test]
    fn a_file_listed_twice_is_an_error() {
        let yaml = "site:\n  name: E\n  url: https://e.com\n  publish:\n    a: /w/a.kame\n    b: /w/a.kame\n";
        assert!(errors(yaml).contains("/w/a.kame"));
    }

    #[test]
    fn bad_slugs_are_errors_all_at_once() {
        let yaml = "site:\n  name: E\n  url: https://e.com\n  publish:\n    Bad: /w/a.kame\n    has space: /w/b.kame\n    ../up: /w/c.kame\n";
        let found = errors(yaml);
        for slug in ["Bad", "has space", "../up"] {
            assert!(found.contains(slug), "{found}");
        }
    }

    #[test]
    fn a_typo_in_a_key_is_an_error() {
        let yaml = "site:\n  name: E\n  url: https://e.com\n  publsh:\n    a: /w/a.kame\n";
        assert!(errors(yaml).contains("publsh"));
    }

    #[test]
    fn name_url_and_publish_are_required() {
        assert!(errors("site:\n  url: https://e.com\n  publish: {}\n").contains("name"));
        assert!(errors("site:\n  name: E\n  publish: {}\n").contains("url"));
        assert!(errors("site:\n  name: E\n  url: https://e.com\n").contains("publish"));
    }

    #[test]
    fn a_missing_config_says_where_it_looked() {
        let found = Config::load(Path::new("/no/such/site"))
            .unwrap_err()
            .join("\n");
        assert!(found.contains("/no/such/site/site.yaml"), "{found}");
    }
}
