//! Small text helpers shared by the parsers.

/// Longest value, owner, or detail string kept in a fact.
pub const MAX_FACT_TEXT_BYTES: usize = 256;

/// Cuts `s` to [`MAX_FACT_TEXT_BYTES`] on a character boundary, marking the cut.
pub fn clip(s: &mut String) {
    if s.len() > MAX_FACT_TEXT_BYTES {
        lyra_protocol::error::truncate_utf8(s, MAX_FACT_TEXT_BYTES - 3);
        s.push_str("...");
    }
}

/// Parent directory of a root-relative path; `""` for the root.
pub fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(d, _)| d)
}

pub fn file_name(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(rel, |(_, f)| f)
}

/// Joins a root-relative directory and a name.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

pub fn shown_dir(dir: &str) -> &str {
    if dir.is_empty() { "the root" } else { dir }
}

/// Maps byte offsets to 1-based line numbers.
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    pub fn line(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    /// Inclusive line range of the byte range `start..end`.
    pub fn lines(&self, start: usize, end: usize) -> crate::Lines {
        let last = end.saturating_sub(1).max(start);
        crate::Lines {
            start: self.line(start),
            end: self.line(last),
        }
    }
}
