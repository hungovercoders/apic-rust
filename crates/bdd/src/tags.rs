//! Tag expressions: `,` is or, `&&` is and, `~` negates, `@` is optional.

use crate::gherkin::Pickle;

/// Keeps the pickles a tag expression selects; an empty expression keeps
/// them all.
pub(crate) fn apply_tag_filter(filter: &str, pickles: Vec<Pickle>) -> Vec<Pickle> {
    if filter.is_empty() {
        return pickles;
    }
    pickles
        .into_iter()
        .filter(|p| matches(filter, &p.tags))
        .collect()
}

fn matches(filters: &str, tags: &[String]) -> bool {
    let mut ok = false;
    for filter in filters.split(',') {
        ok = match_and(filter, tags) || ok;
    }
    ok
}

fn match_and(filter: &str, tags: &[String]) -> bool {
    let mut and = !filter.is_empty();
    for tag in filter.split("&&") {
        let tag = tag.trim().replace('@', "");
        match tag.strip_prefix('~') {
            Some(negated) => and = !contains(tags, negated) && and,
            None => and = contains(tags, &tag) && and,
        }
    }
    and
}

fn contains(tags: &[String], tag: &str) -> bool {
    tags.iter().any(|t| t.replace('@', "") == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn expressions() {
        let t = tags(&["@smoke", "@fast"]);
        assert!(matches("@smoke", &t));
        assert!(matches("smoke && @fast", &t));
        assert!(!matches("@smoke && ~@fast", &t));
        assert!(matches("@slow,@fast", &t));
        assert!(matches("~@slow", &t));
        assert!(!matches("@slow", &t));
        assert!(!matches("@slow", &[]));
    }
}
