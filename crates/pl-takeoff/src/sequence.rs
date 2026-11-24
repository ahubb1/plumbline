//! Sequences: automatic numbering of labels (`D-01`, `D-02`, ...), as pure functions.

use serde::{Deserialize, Serialize};

/// A numbering scheme: `prefix`, a number that starts at `start` and goes up by `step`, padded
/// with zeros to `digits` digits, then `suffix`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sequence {
    #[serde(default)]
    pub prefix: String,
    pub start: i64,
    pub step: i64,
    /// The least number of digits; shorter numbers get leading zeros. 0 or 1 for none.
    #[serde(default)]
    pub digits: u8,
    #[serde(default)]
    pub suffix: String,
}

impl Default for Sequence {
    fn default() -> Sequence {
        Sequence::new("", 1, 1)
    }
}

impl Sequence {
    pub fn new(prefix: &str, start: i64, step: i64) -> Sequence {
        Sequence {
            prefix: prefix.to_string(),
            start,
            step,
            digits: 0,
            suffix: String::new(),
        }
    }

    pub fn with_digits(mut self, digits: u8) -> Sequence {
        self.digits = digits;
        self
    }

    pub fn with_suffix(mut self, suffix: &str) -> Sequence {
        self.suffix = suffix.to_string();
        self
    }

    /// The number of the `index`-th label (from 0), saturating at the ends of `i64`.
    pub fn number(&self, index: usize) -> i64 {
        let i = i64::try_from(index).unwrap_or(i64::MAX);
        self.start.saturating_add(i.saturating_mul(self.step))
    }

    /// The label for a number.
    pub fn format(&self, n: i64) -> String {
        let width = usize::from(self.digits.min(20));
        let digits = format!("{:0width$}", n.unsigned_abs());
        let sign = if n < 0 { "-" } else { "" };
        format!("{}{sign}{digits}{}", self.prefix, self.suffix)
    }

    /// The `index`-th label, from 0.
    pub fn label(&self, index: usize) -> String {
        self.format(self.number(index))
    }

    /// The first `count` labels.
    pub fn labels(&self, count: usize) -> Vec<String> {
        (0..count).map(|i| self.label(i)).collect()
    }

    /// The number a label of this sequence carries; `None` for a label that does not have
    /// the prefix and suffix around a whole number. Case is ignored in the prefix and suffix.
    pub fn parse(&self, label: &str) -> Option<i64> {
        let l = label.trim();
        let lower = l.to_lowercase();
        let (p, s) = (self.prefix.to_lowercase(), self.suffix.to_lowercase());
        if !lower.starts_with(&p) || !lower.ends_with(&s) || lower.len() < p.len() + s.len() {
            return None;
        }
        let middle = lower.get(p.len()..lower.len() - s.len())?;
        let digits = middle.strip_prefix('-').unwrap_or(middle);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        middle.parse().ok()
    }

    /// The label that continues a sequence already used in `existing`: the number after the
    /// furthest one in the direction of `step` (the highest for a rising sequence), or the
    /// first label when none of `existing` belongs to the sequence. Labels that do not belong
    /// are ignored.
    pub fn next<'a>(&self, existing: impl IntoIterator<Item = &'a str>) -> String {
        let used = existing.into_iter().filter_map(|l| self.parse(l));
        let furthest = if self.step >= 0 {
            used.max()
        } else {
            used.min()
        };
        match furthest {
            Some(n) => self.format(n.saturating_add(self.step)),
            None => self.label(0),
        }
    }

    /// Numbers `count` new items after `existing`, as [`Sequence::next`] would one at a time.
    pub fn continue_from<'a>(
        &self,
        existing: impl IntoIterator<Item = &'a str>,
        count: usize,
    ) -> Vec<String> {
        let first = self.next(existing);
        let start = self.parse(&first).unwrap_or(self.start);
        let rest = Sequence {
            start,
            ..self.clone()
        };
        rest.labels(count)
    }
}
