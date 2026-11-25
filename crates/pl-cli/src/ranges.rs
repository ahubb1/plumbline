//! Page ranges as a person writes them: `1-3,5`, `4-` (to the last page), `last`, `all`,
//! `6-4` (backwards). Page numbers start at 1; the parsed result is zero-based.

use anyhow::{bail, Result};

/// One item of a range list, before it is checked against a page count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    Page(usize),
    Last,
}

impl Bound {
    fn parse(text: &str, whole: &str) -> Result<Bound> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("last") || text.eq_ignore_ascii_case("end") {
            return Ok(Bound::Last);
        }
        match text.parse::<usize>() {
            Ok(0) => bail!("page 0 in {whole:?}: pages are numbered from 1"),
            Ok(n) => Ok(Bound::Page(n)),
            Err(_) => bail!("{text:?} in {whole:?} is not a page number"),
        }
    }

    fn resolve(self, count: usize, whole: &str) -> Result<usize> {
        let n = match self {
            Bound::Last => count,
            Bound::Page(n) => n,
        };
        if n > count {
            bail!(
                "page {n} in {whole:?} is out of range: the document has {count} page{}",
                plural(count)
            );
        }
        Ok(n - 1)
    }
}

pub fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The pieces of a range list, each a first and last page (inclusive, zero-based, possibly
/// backwards), checked against `count` pages.
pub fn parse_parts(text: &str, count: usize) -> Result<Vec<(usize, usize)>> {
    let whole = text.trim();
    if whole.is_empty() {
        bail!("the page range is empty");
    }
    if whole.eq_ignore_ascii_case("all") {
        if count == 0 {
            bail!("the document has no pages");
        }
        return Ok(vec![(0, count - 1)]);
    }
    let mut parts = Vec::new();
    for item in whole.split(',') {
        let item = item.trim();
        if item.is_empty() {
            bail!("{whole:?} has an empty item");
        }
        let (first, last) = match item.split_once('-') {
            Some((a, b)) => {
                let a = if a.trim().is_empty() {
                    Bound::Page(1)
                } else {
                    Bound::parse(a, whole)?
                };
                let b = if b.trim().is_empty() {
                    Bound::Last
                } else {
                    Bound::parse(b, whole)?
                };
                (a, b)
            }
            None => {
                let a = Bound::parse(item, whole)?;
                (a, a)
            }
        };
        parts.push((first.resolve(count, whole)?, last.resolve(count, whole)?));
    }
    Ok(parts)
}

/// A range list as zero-based pages in the order written. A page may not appear twice.
pub fn parse_pages(text: &str, count: usize) -> Result<Vec<usize>> {
    let mut pages = Vec::new();
    for (a, b) in parse_parts(text, count)? {
        if a <= b {
            pages.extend(a..=b);
        } else {
            pages.extend((b..=a).rev());
        }
    }
    let mut seen = vec![false; count];
    for p in &pages {
        if std::mem::replace(&mut seen[*p], true) {
            bail!("page {} appears twice in {:?}", p + 1, text.trim());
        }
    }
    Ok(pages)
}

/// A one-based position from 1 to `max` (`last`/`end` is `max`), as a zero-based index.
pub fn parse_position(text: &str, max: usize, what: &str) -> Result<usize> {
    let t = text.trim();
    let n = if t.eq_ignore_ascii_case("last") || t.eq_ignore_ascii_case("end") {
        max
    } else {
        match t.parse::<usize>() {
            Ok(n) => n,
            Err(_) => bail!("{what} {t:?} is not a page number"),
        }
    };
    if n == 0 || n > max {
        bail!("{what} {n} is out of range: it must be from 1 to {max}");
    }
    Ok(n - 1)
}

/// Zero-based pages as the shortest range list: `1-3,5`.
pub fn format_pages(pages: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let mut j = i;
        while j + 1 < pages.len() && pages[j + 1] == pages[j] + 1 {
            j += 1;
        }
        if j == i {
            out.push(format!("{}", pages[i] + 1));
        } else {
            out.push(format!("{}-{}", pages[i] + 1, pages[j] + 1));
        }
        i = j + 1;
    }
    out.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse_as_written() {
        assert_eq!(parse_pages("1-3,5", 8).unwrap(), vec![0, 1, 2, 4]);
        assert_eq!(parse_pages("6-", 8).unwrap(), vec![5, 6, 7]);
        assert_eq!(parse_pages("-2", 8).unwrap(), vec![0, 1]);
        assert_eq!(parse_pages("last", 8).unwrap(), vec![7]);
        assert_eq!(parse_pages("all", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("3-1", 3).unwrap(), vec![2, 1, 0]);
        assert_eq!(parse_pages(" 2 , 4 - 5 ", 8).unwrap(), vec![1, 3, 4]);
        assert!(parse_pages("0", 8).is_err());
        assert!(parse_pages("9", 8).is_err());
        assert!(parse_pages("1,1", 8).is_err());
        assert!(parse_pages("1-3,2", 8).is_err());
        assert!(parse_pages("a", 8).is_err());
        assert!(parse_pages("", 8).is_err());
        assert!(parse_pages("1,,2", 8).is_err());
        assert_eq!(parse_parts("1-3,4-8", 8).unwrap(), vec![(0, 2), (3, 7)]);
    }

    #[test]
    fn positions_and_formatting() {
        assert_eq!(parse_position("1", 5, "at").unwrap(), 0);
        assert_eq!(parse_position("end", 5, "at").unwrap(), 4);
        assert!(parse_position("6", 5, "at").is_err());
        assert!(parse_position("0", 5, "at").is_err());
        assert_eq!(format_pages(&[0, 1, 2, 4, 6, 7]), "1-3,5,7-8");
        assert_eq!(format_pages(&[3]), "4");
        assert_eq!(format_pages(&[]), "");
    }
}
