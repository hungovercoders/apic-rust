//! Ranks the names a user probably meant when the one they typed matches
//! nothing: request ids, environment names and the like.

#![forbid(unsafe_code)]

use std::collections::HashSet;

/// How many names [`closest`] returns at most: enough to show the likely
/// intended one, few enough to read in an error line.
pub const MAX: usize = 3;

/// Returns up to [`MAX`] of `candidates` that are close to `query`, best
/// first: a candidate `query` is a prefix of, then one `query` occurs in,
/// then one within a few edits of it (one edit for a short query, three
/// for a long one). Case, and the difference between `-` and `_`, do not
/// count. It returns nothing when nothing is close enough, or the query is
/// a single letter: a wrong name should not get a guess for the sake of
/// it.
pub fn closest<S: AsRef<str>>(query: &str, candidates: &[S]) -> Vec<String> {
    let q = normalize(query);
    let n = q.chars().count();
    if n < 2 {
        return Vec::new(); // one letter is inside half the names
    }
    let limit = (1 + n / 5).min(3);
    // (rank, distance, name): rank 0 is a prefix, 1 contains, 2 edits.
    let mut found: Vec<(u8, usize, &str)> = Vec::new();
    let mut seen = HashSet::new();
    for c in candidates {
        let c = c.as_ref();
        if !seen.insert(c) {
            continue;
        }
        let name = normalize(c);
        let extra = name.chars().count().saturating_sub(n);
        if name == q {
            found.push((0, 0, c));
        } else if name.starts_with(&q) {
            found.push((0, extra, c));
        } else if name.contains(&q) {
            found.push((1, extra, c));
        } else {
            let d = distance(&q, &name);
            if d <= limit && d < n {
                found.push((2, d, c));
            }
        }
    }
    found.sort();
    found.truncate(MAX);
    found
        .into_iter()
        .map(|(_, _, name)| name.to_string())
        .collect()
}

/// Renders names as "did you mean a, b or c?", or `""` for none.
pub fn phrase<S: AsRef<str>>(names: &[S]) -> String {
    let names: Vec<&str> = names.iter().map(AsRef::as_ref).collect();
    match names.as_slice() {
        [] => String::new(),
        [only] => format!("did you mean {only}?"),
        [first @ .., last] => format!("did you mean {} or {last}?", first.join(", ")),
    }
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase().replace('_', "-")
}

/// The Levenshtein edit distance between `a` and `b`, by characters.
fn distance(a: &str, b: &str) -> usize {
    let ra: Vec<char> = a.chars().collect();
    let rb: Vec<char> = b.chars().collect();
    if ra.is_empty() {
        return rb.len();
    }
    if rb.is_empty() {
        return ra.len();
    }
    let mut prev: Vec<usize> = (0..=rb.len()).collect();
    let mut cur = vec![0; rb.len() + 1];
    for i in 1..=ra.len() {
        cur[0] = i;
        for j in 1..=rb.len() {
            let cost = usize::from(ra[i - 1] != rb[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[rb.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_cases() {
        let ids = [
            "login",
            "whoami",
            "get-user",
            "get-users",
            "list-users",
            "delete-user",
            "health",
        ];
        let cases: &[(&str, &[&str])] = &[
            ("get-usr", &["get-user", "get-users"]),  // one edit
            ("get", &["get-user", "get-users"]),      // prefix
            ("users", &["get-users", "list-users"]),  // contains
            ("get_user", &["get-user", "get-users"]), // _ reads as -
            ("Login", &["login"]),                    // case
            ("lgoin", &["login"]), // transposition is two edits, allowed at length 5
            ("whomai", &["whoami"]),
            ("delete-usre", &["delete-user"]),
            ("xyz", &[]), // nothing close
            ("", &[]),
            ("a", &[]),              // one letter would match everything within one edit
            ("é", &[]),              // one letter in two bytes, the same
            ("héalth", &["health"]), // one edit, counted in characters
            ("user", &["get-user", "get-users", "list-users"]), // capped at MAX, shortest first
        ];
        for (query, want) in cases {
            assert_eq!(closest(query, &ids), *want, "closest({query:?})");
        }
    }

    #[test]
    fn closest_keeps_the_exact_spelling_and_dedupes() {
        let got = closest("stagin", &["staging", "Staging", "staging", "prod"]);
        assert_eq!(got, ["Staging", "staging"]);
    }

    #[test]
    fn phrase_cases() {
        let cases: &[(&str, &[&str])] = &[
            ("", &[]),
            ("did you mean a?", &["a"]),
            ("did you mean a or b?", &["a", "b"]),
            ("did you mean a, b or c?", &["a", "b", "c"]),
        ];
        for (want, names) in cases {
            assert_eq!(phrase(names), *want, "phrase({names:?})");
        }
        assert_eq!(phrase(&[String::from("a")]), "did you mean a?");
    }

    #[test]
    fn distance_cases() {
        let cases = [
            ("", "", 0),
            ("", "abc", 3),
            ("abc", "", 3),
            ("abc", "abc", 0),
            ("kitten", "sitting", 3),
            ("flaw", "lawn", 2),
            ("héllo", "hello", 1),
        ];
        for (a, b, want) in cases {
            assert_eq!(distance(a, b), want, "distance({a:?}, {b:?})");
        }
    }
}
