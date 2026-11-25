//! Pure search/filter/sort logic for the TUI (DESIGN §8.3).

use crate::model::State;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    Free,
    Busy,
    Unknown,
    Error,
}

impl StatusFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Free,
            Self::Free => Self::Busy,
            Self::Busy => Self::Unknown,
            Self::Unknown => Self::Error,
            Self::Error => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Free => "free",
            Self::Busy => "busy",
            Self::Unknown => "unknown",
            Self::Error => "error",
        }
    }

    pub fn matches_state(self, state: State) -> bool {
        match self {
            Self::All => true,
            Self::Free => state == State::Free,
            Self::Busy => state == State::Busy,
            Self::Unknown => state == State::Unknown,
            Self::Error => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortMode {
    #[default]
    Host,
    FreeMem,
}

impl SortMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::Host => Self::FreeMem,
            Self::FreeMem => Self::Host,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::FreeMem => "free-mem",
        }
    }
}

/// Case-insensitive subsequence match, e.g. `g06` matches `gpu06`.
pub fn subsequence(pattern: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars().flat_map(char::to_lowercase);
    pattern
        .chars()
        .flat_map(char::to_lowercase)
        .all(|needle| chars.any(|c| c == needle))
}

pub fn tokenize(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_string).collect()
}

/// Every whitespace-separated token must match at least one haystack field.
pub fn matches(tokens: &[String], haystacks: &[&str]) -> bool {
    tokens
        .iter()
        .all(|token| haystacks.iter().any(|field| subsequence(token, field)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_is_case_insensitive() {
        assert!(subsequence("g06", "GPU06"));
        assert!(subsequence("PY", "python"));
        assert!(!subsequence("g60", "gpu06"));
        assert!(subsequence("", "anything"));
    }

    #[test]
    fn all_tokens_must_hit_some_field() {
        let tokens = tokenize("gpu py");
        assert!(matches(&tokens, &["gpu06", "python train"]));
        assert!(!matches(&tokens, &["gpu06", "train"]));
    }

    #[test]
    fn status_filter_cycles_through_every_state() {
        let mut f = StatusFilter::All;
        let mut seen = vec![f];
        for _ in 0..4 {
            f = f.next();
            seen.push(f);
        }
        assert_eq!(
            seen,
            vec![
                StatusFilter::All,
                StatusFilter::Free,
                StatusFilter::Busy,
                StatusFilter::Unknown,
                StatusFilter::Error,
            ]
        );
        assert_eq!(f.next(), StatusFilter::All);
        assert!(StatusFilter::Free.matches_state(State::Free));
        assert!(!StatusFilter::Free.matches_state(State::Busy));
        assert!(!StatusFilter::Error.matches_state(State::Free));
    }

    #[test]
    fn sort_toggles() {
        assert_eq!(SortMode::Host.toggled(), SortMode::FreeMem);
        assert_eq!(SortMode::FreeMem.toggled(), SortMode::Host);
    }
}
