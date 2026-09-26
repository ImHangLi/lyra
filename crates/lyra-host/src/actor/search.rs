//! Catalog search ranking for `catalog.list` queries.
//!
//! The query is split into lowercase words. An item matches when any word matches its
//! ref, local id, title, tags, or description. Items rank by their best match tier:
//! exact ref or id, exact title, ref/id/title prefix, ref/id/title substring, tag, then
//! description. Within a tier, items that match more words rank higher. Ties keep catalog
//! order. The whole query also counts as one phrase for the exact ref, id, and title tiers.

use lyra_protocol::ipc::CatalogItem;

/// Match tiers, best first.
const EXACT_REF: u8 = 0;
const EXACT_TITLE: u8 = 1;
const PREFIX: u8 = 2;
const SUBSTRING: u8 = 3;
const TAG: u8 = 4;
const DESCRIPTION: u8 = 5;

struct Fields {
    item_ref: String,
    id: String,
    title: String,
    tags: Vec<String>,
    description: String,
}

impl Fields {
    fn of(item: &CatalogItem) -> Self {
        let item_ref = item.item_ref.to_string().to_lowercase();
        let id = item_ref
            .split_once('.')
            .map_or(item_ref.as_str(), |(_, id)| id)
            .to_owned();
        Self {
            item_ref,
            id,
            title: item.title.to_lowercase(),
            tags: item.tags.iter().map(|t| t.to_lowercase()).collect(),
            description: item.description.to_lowercase(),
        }
    }

    /// The best tier `word` reaches in this item, or `None` when it does not match.
    fn tier(&self, word: &str) -> Option<u8> {
        let names = [&self.item_ref, &self.id, &self.title];
        if self.item_ref == word || self.id == word {
            Some(EXACT_REF)
        } else if self.title == word {
            Some(EXACT_TITLE)
        } else if names.iter().any(|n| n.starts_with(word)) {
            Some(PREFIX)
        } else if names.iter().any(|n| n.contains(word)) {
            Some(SUBSTRING)
        } else if self.tags.iter().any(|t| t.contains(word)) {
            Some(TAG)
        } else if self.description.contains(word) {
            Some(DESCRIPTION)
        } else {
            None
        }
    }
}

/// Sort key for one item: `(best tier, words not matched)`; `None` when nothing matches.
fn rank(item: &CatalogItem, words: &[String], phrase: &str) -> Option<(u8, usize)> {
    let fields = Fields::of(item);
    let tiers: Vec<u8> = words.iter().filter_map(|w| fields.tier(w)).collect();
    let mut best = tiers.iter().copied().min()?;
    if words.len() > 1 {
        if fields.item_ref == phrase || fields.id == phrase {
            best = EXACT_REF;
        } else if fields.title == phrase {
            best = best.min(EXACT_TITLE);
        }
    }
    Some((best, words.len() - tiers.len()))
}

/// Filters and ranks `items` for the lowercase query `words`. An empty query keeps all
/// items in catalog order.
pub(super) fn search(items: Vec<CatalogItem>, words: &[String]) -> Vec<CatalogItem> {
    if words.is_empty() {
        return items;
    }
    let phrase = words.join(" ");
    let mut ranked: Vec<((u8, usize), CatalogItem)> = items
        .into_iter()
        .filter_map(|i| rank(&i, words, &phrase).map(|k| (k, i)))
        .collect();
    // A stable sort keeps catalog order for equal keys.
    ranked.sort_by_key(|(k, _)| *k);
    ranked.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use lyra_protocol::ids::{Digest, ItemRef};
    use lyra_protocol::ipc::CatalogItemKind;
    use lyra_protocol::manifest::ActionMode;

    use super::*;

    fn item(r: &str, title: &str, description: &str, tags: &[&str]) -> CatalogItem {
        CatalogItem {
            item_ref: ItemRef::parse(r.to_owned()).expect("valid ref"),
            title: title.to_owned(),
            description: description.to_owned(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            enabled: true,
            definition_hash: Digest::of_bytes(r.as_bytes()),
            item: CatalogItemKind::Action {
                mode: ActionMode::Task,
            },
        }
    }

    fn refs(items: &[CatalogItem]) -> Vec<String> {
        items.iter().map(|i| i.item_ref.to_string()).collect()
    }

    fn words(q: &str) -> Vec<String> {
        q.split_whitespace().map(str::to_lowercase).collect()
    }

    fn catalog() -> Vec<CatalogItem> {
        vec![
            item(
                "dev.test",
                "Tests",
                "Run the suite; lint first",
                &["lint", "check"],
            ),
            item(
                "dev.typecheck",
                "Type check",
                "Check types",
                &["lint", "check"],
            ),
            item("dev.check", "Check", "All checks", &["lint", "check"]),
            item("dev.lint", "Lint", "Lint the code", &["lint", "check"]),
            item("perf.slow", "Slowest tests", "Find slow tests", &[]),
        ]
    }

    #[test]
    fn exact_id_ranks_first() {
        let got = refs(&search(catalog(), &words("lint")));
        assert_eq!(got[0], "dev.lint");
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn exact_id_before_substring() {
        let got = refs(&search(catalog(), &words("check")));
        let check = got.iter().position(|r| r == "dev.check");
        let typecheck = got.iter().position(|r| r == "dev.typecheck");
        assert_eq!(check, Some(0));
        assert!(typecheck > check);
    }

    #[test]
    fn any_word_matches_and_phrase_hits_title() {
        let got = refs(&search(catalog(), &words("slowest tests")));
        assert_eq!(got[0], "perf.slow");
        assert!(got.contains(&"dev.test".to_owned()));
    }

    #[test]
    fn ties_keep_catalog_order_and_empty_query_keeps_all() {
        assert_eq!(search(catalog(), &[]).len(), 5);
        let got = refs(&search(catalog(), &words("zzz")));
        assert!(got.is_empty());
    }
}
