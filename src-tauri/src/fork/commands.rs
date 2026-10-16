//! The server's command table: every name in `run`'s `generate_handler!`
//! list, read from `lib.rs` at compile time. Upstream adding, renaming or
//! dropping a command changes this table with no fork edit.

use std::sync::LazyLock;

const LIB_RS: &str = include_str!("../lib.rs");

static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| parse(LIB_RS));

/// The commands a client may invoke, in `lib.rs` order.
#[cfg(test)]
pub fn names() -> &'static [&'static str] {
    &NAMES
}

pub fn exists(name: &str) -> bool {
    NAMES.contains(&name)
}

/// The last path segment of each entry between `generate_handler![` and `]`.
fn parse(source: &'static str) -> Vec<&'static str> {
    let Some(start) = source.find("generate_handler![") else { return Vec::new() };
    let body = &source[start + "generate_handler![".len()..];
    let body = &body[..body.find(']').unwrap_or(0)];
    body.lines()
        .map(|line| line.split("//").next().unwrap_or("").trim().trim_end_matches(','))
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.rsplit("::").next().unwrap_or(entry))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_handler_list() {
        let names = names();
        assert!(names.len() > 150, "only {} commands parsed", names.len());
        for expected in ["playlist_tree", "fetch_rows", "create_playlist", "test_eval_result"] {
            assert!(exists(expected), "{expected} missing from the table");
        }
        assert!(names.iter().all(|n| n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')));
    }
}
