//! Turning the names a person uses into the ids the database uses (FR-5.3).
//!
//! The model never sees a UUID, so every tool that takes an account, category
//! or payee takes a *name*. Names are typed loosely — "checking" for an account
//! whose real name carries a bank prefix and an account-number suffix,
//! "grocries" for "Groceries" — so matching is layered, and an ambiguous result
//! is handed back as candidates rather than guessed at.

use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Serialize;

/// Ambiguous matches beyond this are noise; the caller needs enough to choose
/// between, not an inventory.
const MAX_CANDIDATES: usize = 10;

/// Suggestions offered when nothing matched.
const MAX_SUGGESTIONS: usize = 5;

/// Something that can be named: an id, and the name a person would type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Named {
    pub id: String,
    pub name: String,
}

/// The outcome of a name lookup.
///
/// Note that nothing here is an error. A name that matches several things is a
/// *result* the model can act on — it can pick one and call again — whereas a
/// protocol error would just fail the tool call and tell it nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Exactly one match.
    One(Named),
    /// Several equally good matches, best-first, capped at [`MAX_CANDIDATES`].
    Ambiguous(Vec<Named>),
    /// Nothing matched. `suggestions` is a best-effort shortlist.
    Nothing { suggestions: Vec<Named> },
}

impl Resolution {
    pub fn one(self) -> Option<Named> {
        match self {
            Self::One(named) => Some(named),
            _ => None,
        }
    }

    /// A sentence the model can act on, for tools that report the miss inline.
    pub fn describe(&self, kind: &str, query: &str) -> String {
        match self {
            Self::One(n) => format!("{kind} {:?} resolved to {:?}", query, n.name),
            Self::Ambiguous(cands) => format!(
                "{:?} matches several {kind}s: {}. Call again with one of these exact names.",
                query,
                names(cands)
            ),
            Self::Nothing { suggestions } if suggestions.is_empty() => {
                format!("No {kind} matches {query:?}.")
            }
            Self::Nothing { suggestions } => format!(
                "No {kind} matches {:?}. Did you mean: {}?",
                query,
                names(suggestions)
            ),
        }
    }
}

fn names(items: &[Named]) -> String {
    items
        .iter()
        .map(|n| format!("{:?}", n.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Functional core: match a typed name against candidates. No database.
///
/// Tiers are tried in order and the first that produces any match wins, so an
/// exact hit is never diluted by loose substring matches elsewhere.
pub fn resolve(candidates: &[Named], query: &str) -> Resolution {
    let needle = normalise(query);
    if needle.is_empty() {
        return Resolution::Nothing {
            suggestions: Vec::new(),
        };
    }

    let prepared: Vec<(String, &Named)> =
        candidates.iter().map(|c| (normalise(&c.name), c)).collect();

    for tier in [Tier::Exact, Tier::Prefix, Tier::Substring, Tier::Word] {
        let hits: Vec<Named> = prepared
            .iter()
            .filter(|(name, _)| tier.matches(name, &needle))
            .map(|(_, c)| (*c).clone())
            .collect();

        match hits.len() {
            0 => continue,
            1 => return Resolution::One(hits.into_iter().next().expect("len checked")),
            _ => {
                let mut hits = hits;
                hits.truncate(MAX_CANDIDATES);
                return Resolution::Ambiguous(hits);
            }
        }
    }

    Resolution::Nothing {
        suggestions: suggest(&prepared, &needle),
    }
}

#[derive(Clone, Copy)]
enum Tier {
    Exact,
    Prefix,
    Substring,
    /// Any whitespace-separated word of the candidate starts with the needle,
    /// so "fitness" finds "Health & Fitness".
    Word,
}

impl Tier {
    fn matches(self, name: &str, needle: &str) -> bool {
        match self {
            Self::Exact => name == needle,
            Self::Prefix => name.starts_with(needle),
            Self::Substring => name.contains(needle),
            Self::Word => name.split_whitespace().any(|w| w.starts_with(needle)),
        }
    }
}

/// Lowercase, trim, and collapse internal whitespace.
fn normalise(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Nearest names by edit distance, for typos like "grocries".
fn suggest(prepared: &[(String, &Named)], needle: &str) -> Vec<Named> {
    let tolerance = (needle.chars().count() / 3).max(2);

    let mut scored: Vec<(usize, &Named)> = prepared
        .iter()
        .filter_map(|(name, c)| {
            let d = edit_distance(name, needle);
            (d <= tolerance).then_some((d, *c))
        })
        .collect();

    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(_, c)| c.clone())
        .collect()
}

/// Levenshtein distance, two rows at a time.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitution.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    prev[b.len()]
}

// ------------------------------------------------------------- loaders -----

/// Live accounts. Closed ones are included: resolving a name is not the same
/// as choosing to report on it, and the caller's `TxScope` decides that.
pub fn accounts(conn: &Connection) -> Result<Vec<Named>, rusqlite::Error> {
    load(
        conn,
        "SELECT id, name FROM accounts WHERE tombstone = 0 ORDER BY name",
    )
}

/// Live categories, hidden ones included for the same reason.
///
/// `v_categories` does *not* filter tombstones the way `v_transactions` does,
/// so the predicate is ours to write.
pub fn categories(conn: &Connection) -> Result<Vec<Named>, rusqlite::Error> {
    load(
        conn,
        "SELECT id, name FROM v_categories WHERE tombstone = 0 ORDER BY name",
    )
}

/// Live payees. `v_payees` renames transfer payees to their account, which is
/// what a person would call them.
pub fn payees(conn: &Connection) -> Result<Vec<Named>, rusqlite::Error> {
    load(
        conn,
        "SELECT id, name FROM v_payees WHERE tombstone = 0 ORDER BY name",
    )
}

fn load(conn: &Connection, sql: &str) -> Result<Vec<Named>, rusqlite::Error> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(Named {
            id: r.get(0)?,
            name: r.get(1)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, name: &str) -> Named {
        Named {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    /// Shaped the way bank-imported accounts usually are: an institution
    /// prefix and an account-number suffix around the useful word.
    fn accounts_fixture() -> Vec<Named> {
        vec![
            n("a1", "Example Bank Checking (0001)"),
            n("a2", "Example Bank Savings (0002)"),
            n("a3", "Example Rewards Card (0003)"),
            n("a4", "Other Bank Credit Card (0004)"),
            n("a5", "Example Retirement"),
        ]
    }

    fn categories_fixture() -> Vec<Named> {
        vec![
            n("c1", "Groceries"),
            n("c2", "Health & Fitness"),
            n("c3", "Home"),
            n("c4", "Shopping"),
            n("c5", "Savings and Investments"),
        ]
    }

    #[test]
    fn exact_match_wins() {
        let got = resolve(&categories_fixture(), "Groceries");
        assert_eq!(got, Resolution::One(n("c1", "Groceries")));
    }

    #[test]
    fn matching_ignores_case_and_padding() {
        assert_eq!(
            resolve(&categories_fixture(), "  gROCERIES "),
            Resolution::One(n("c1", "Groceries"))
        );
    }

    /// Nobody types the full imported account name.
    #[test]
    fn substring_finds_an_account_by_its_useful_part() {
        assert_eq!(
            resolve(&accounts_fixture(), "checking"),
            Resolution::One(n("a1", "Example Bank Checking (0001)"))
        );
    }

    #[test]
    fn a_word_prefix_matches_mid_name() {
        assert_eq!(
            resolve(&categories_fixture(), "fitness"),
            Resolution::One(n("c2", "Health & Fitness"))
        );
    }

    /// FR-5.3: hand back the candidates instead of picking one.
    #[test]
    fn several_matches_are_ambiguous_not_guessed() {
        match resolve(&accounts_fixture(), "example bank") {
            Resolution::Ambiguous(c) => {
                assert_eq!(c.len(), 2);
                assert!(c.iter().any(|x| x.id == "a1"));
                assert!(c.iter().any(|x| x.id == "a2"));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    /// An exact hit must not be diluted by looser matches elsewhere.
    #[test]
    fn an_exact_match_beats_substring_matches() {
        let candidates = vec![
            n("x1", "Home"),
            n("x2", "Home Improvement"),
            n("x3", "Homewares"),
        ];
        assert_eq!(
            resolve(&candidates, "Home"),
            Resolution::One(n("x1", "Home"))
        );
    }

    #[test]
    fn typos_produce_suggestions() {
        match resolve(&categories_fixture(), "grocries") {
            Resolution::Nothing { suggestions } => {
                assert_eq!(
                    suggestions.first().map(|s| s.name.as_str()),
                    Some("Groceries")
                );
            }
            other => panic!("expected Nothing with suggestions, got {other:?}"),
        }
    }

    #[test]
    fn nonsense_yields_nothing_at_all() {
        match resolve(&categories_fixture(), "zzzzzzzzzzzz") {
            Resolution::Nothing { suggestions } => assert!(suggestions.is_empty()),
            other => panic!("expected empty Nothing, got {other:?}"),
        }
    }

    #[test]
    fn empty_query_matches_nothing() {
        assert_eq!(
            resolve(&categories_fixture(), "   "),
            Resolution::Nothing {
                suggestions: vec![]
            }
        );
    }

    /// A very loose query must not dump the whole payee list into the response.
    #[test]
    fn ambiguous_results_are_capped() {
        let many: Vec<Named> = (0..50)
            .map(|i| n(&format!("p{i}"), &format!("Store {i}")))
            .collect();
        match resolve(&many, "store") {
            Resolution::Ambiguous(c) => assert_eq!(c.len(), MAX_CANDIDATES),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn describe_tells_the_model_what_to_do() {
        let text = resolve(&accounts_fixture(), "example bank").describe("account", "example bank");
        assert!(text.contains("Example Bank Checking (0001)"), "got {text}");
        assert!(text.contains("Call again"), "got {text}");
    }

    #[test]
    fn edit_distance_is_sane() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("grocries", "groceries"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
