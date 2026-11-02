//! Versions and ranges as Moebius reads them: a version is one to three numbers separated by dots, a range is
//! comparators (`>=`, `>`, `<=`, `<`, `=` or none, then a version) separated by commas or spaces, or `*`.

/// A version of a package or of Moebius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// The version in `text`, or none when it is not one to three numbers separated by dots.
    pub fn parse(text: &str) -> Option<Self> {
        let mut numbers = [0u64; 3];
        for (count, part) in text.trim().split('.').enumerate() {
            if count == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            numbers[count] = part.parse().ok()?;
        }
        Some(Self(numbers[0], numbers[1], numbers[2]))
    }
}

/// Whether `text` is a range of versions.
pub fn valid_range(text: &str) -> bool {
    let mut any = false;
    for word in text.split([',', ' ']).filter(|word| !word.is_empty()) {
        if word == "*" {
            continue;
        }
        let rest = ["<=", ">=", "<", ">", "="]
            .iter()
            .find_map(|op| word.strip_prefix(op))
            .unwrap_or(word);
        if Version::parse(rest).is_none() {
            return false;
        }
        any = true;
    }
    any || text.trim() == "*"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_have_one_to_three_numbers_and_order_by_them() {
        assert_eq!(Version::parse("0.2"), Version::parse("0.2.0"));
        assert!(Version::parse("0.10.0") > Version::parse("0.9.9"));
        for bad in ["", "1.2.3.4", "1.x", "v1", "1..2", "1.2-rc1", "-1"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn ranges_are_comparators_or_a_star() {
        for good in [">=0.2", ">=0.2, <1", ">0.2 <=0.4", "=0.2.1", "0.2.1", "*"] {
            assert!(valid_range(good), "{good}");
        }
        for bad in ["", ">=", ">=x", "^1", "~1.2"] {
            assert!(!valid_range(bad), "{bad}");
        }
    }
}
