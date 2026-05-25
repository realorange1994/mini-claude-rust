//! Semantic version parsing and comparison utilities.
//! Ported from upstream semver.go (228 lines).
//! Ported from upstream TypeScript: src/utils/semver.ts

use std::cmp::Ordering;

/// Represents a parsed semantic version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semver {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
    pub prerelease: String,
}

impl Semver {
    /// Parse a version string like "1.2.3" or "1.2.3-alpha".
    pub fn parse(v: &str) -> Result<Self, String> {
        let v = v.trim();

        let (version_part, prerelease) = if let Some(idx) = v.find('-') {
            (&v[..idx], &v[idx + 1..])
        } else {
            (v, "")
        };

        let parts: Vec<&str> = version_part.splitn(3, '.').collect();
        if parts.len() < 3 {
            return Err(format!("invalid version: {}", v));
        }

        let major: i64 = parts[0]
            .parse()
            .map_err(|_| format!("invalid major version: {}", parts[0]))?;
        let minor: i64 = parts[1]
            .parse()
            .map_err(|_| format!("invalid minor version: {}", parts[1]))?;
        let patch: i64 = parts[2]
            .parse()
            .map_err(|_| format!("invalid patch version: {}", parts[2]))?;

        Ok(Semver {
            major,
            minor,
            patch,
            prerelease: prerelease.to_string(),
        })
    }
}

impl Ord for Semver {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| {
                // Pre-release versions have lower precedence than release versions
                match (&self.prerelease.is_empty(), &other.prerelease.is_empty()) {
                    (true, true) => self.patch.cmp(&other.patch),
                    (false, true) => {
                        // self is pre-release, other is release → self < other
                        self.patch.cmp(&other.patch).then(Ordering::Less)
                    }
                    (true, false) => {
                        // self is release, other is pre-release → self > other
                        self.patch.cmp(&other.patch).then(Ordering::Greater)
                    }
                    (false, false) => self.patch.cmp(&other.patch),
                }
            })
            .then_with(|| self.prerelease.cmp(&other.prerelease))
    }
}

impl PartialOrd for Semver {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Returns true if a > b.
pub fn gt(a: &str, b: &str) -> bool {
    matches!((Semver::parse(a), Semver::parse(b)), (Ok(va), Ok(vb)) if va > vb)
}

/// Returns true if a >= b.
pub fn gte(a: &str, b: &str) -> bool {
    matches!((Semver::parse(a), Semver::parse(b)), (Ok(va), Ok(vb)) if va >= vb)
}

/// Returns true if a < b.
pub fn lt(a: &str, b: &str) -> bool {
    matches!((Semver::parse(a), Semver::parse(b)), (Ok(va), Ok(vb)) if va < vb)
}

/// Returns true if a <= b.
pub fn lte(a: &str, b: &str) -> bool {
    matches!((Semver::parse(a), Semver::parse(b)), (Ok(va), Ok(vb)) if va <= vb)
}

/// Compare two version strings.
/// Returns 1 if a > b, -1 if a < b, 0 if equal.
pub fn order(a: &str, b: &str) -> i32 {
    match (Semver::parse(a), Semver::parse(b)) {
        (Ok(va), Ok(vb)) => match va.cmp(&vb) {
            Ordering::Greater => 1,
            Ordering::Less => -1,
            Ordering::Equal => 0,
        },
        _ => 0,
    }
}

/// Check if a version satisfies a range specification.
/// Supported ranges:
/// - Exact: "1.2.3"
/// - Caret: "^1.2.3" (compatible with 1.x.x, allows minor/patch bumps)
/// - Tilde: "~1.2.3" (allows patch bumps only)
/// - Wildcard: "*" (any version)
/// - Comparison: ">=1.0.0", ">1.0.0", "<=1.0.0", "<1.0.0"
pub fn satisfies(version: &str, range_spec: &str) -> bool {
    let v = match Semver::parse(version) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let range_spec = range_spec.trim();

    // Wildcard
    if range_spec == "*" {
        return true;
    }

    // Caret range: ^1.2.3
    if let Some(rest) = range_spec.strip_prefix('^') {
        if let Ok(base) = Semver::parse(rest) {
            return v.major == base.major && v >= base;
        }
        return false;
    }

    // Tilde range: ~1.2.3
    if let Some(rest) = range_spec.strip_prefix('~') {
        if let Ok(base) = Semver::parse(rest) {
            return v.major == base.major && v.minor == base.minor && v >= base;
        }
        return false;
    }

    // Comparison operators
    if let Some(rest) = range_spec.strip_prefix(">=") {
        if let Ok(base) = Semver::parse(rest) {
            return v >= base;
        }
        return false;
    }
    if let Some(rest) = range_spec.strip_prefix("<=") {
        if let Ok(base) = Semver::parse(rest) {
            return v <= base;
        }
        return false;
    }
    if range_spec.starts_with('>') && !range_spec.starts_with(">=") {
        if let Ok(base) = Semver::parse(&range_spec[1..]) {
            return v > base;
        }
        return false;
    }
    if range_spec.starts_with('<') && !range_spec.starts_with("<=") {
        if let Ok(base) = Semver::parse(&range_spec[1..]) {
            return v < base;
        }
        return false;
    }

    // Exact match
    match Semver::parse(range_spec) {
        Ok(base) => v == base,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_semver() {
        let v = Semver::parse("1.2.3").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert!(v.prerelease.is_empty());
    }

    #[test]
    fn test_parse_semver_prerelease() {
        let v = Semver::parse("1.2.3-alpha").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.prerelease, "alpha");
    }

    #[test]
    fn test_parse_semver_invalid() {
        assert!(Semver::parse("1.2").is_err());
        assert!(Semver::parse("abc.def.ghi").is_err());
    }

    #[test]
    fn test_compare_semver() {
        assert!(gt("2.0.0", "1.0.0"));
        assert!(gt("1.2.0", "1.1.0"));
        assert!(gt("1.1.2", "1.1.1"));
    }

    #[test]
    fn test_compare_semver_prerelease() {
        // Release > pre-release
        assert!(gt("1.0.0", "1.0.0-alpha"));
        assert!(lt("1.0.0-alpha", "1.0.0"));
    }

    #[test]
    fn test_semver_order() {
        assert_eq!(order("1.0.0", "2.0.0"), -1);
        assert_eq!(order("2.0.0", "1.0.0"), 1);
        assert_eq!(order("1.0.0", "1.0.0"), 0);
    }

    #[test]
    fn test_satisfies_exact() {
        assert!(satisfies("1.2.3", "1.2.3"));
        assert!(!satisfies("1.2.4", "1.2.3"));
    }

    #[test]
    fn test_satisfies_wildcard() {
        assert!(satisfies("1.2.3", "*"));
        assert!(satisfies("999.0.0", "*"));
    }

    #[test]
    fn test_satisfies_caret() {
        assert!(satisfies("1.2.3", "^1.2.3"));
        assert!(satisfies("1.3.0", "^1.2.3"));
        assert!(satisfies("1.9.9", "^1.2.3"));
        assert!(!satisfies("2.0.0", "^1.2.3"));
        assert!(!satisfies("1.2.2", "^1.2.3"));
    }

    #[test]
    fn test_satisfies_tilde() {
        assert!(satisfies("1.2.3", "~1.2.3"));
        assert!(satisfies("1.2.9", "~1.2.3"));
        assert!(!satisfies("1.3.0", "~1.2.3"));
        assert!(!satisfies("2.0.0", "~1.2.3"));
    }

    #[test]
    fn test_satisfies_comparison() {
        assert!(satisfies("1.2.4", ">=1.2.3"));
        assert!(!satisfies("1.2.2", ">=1.2.3"));
        assert!(satisfies("1.2.2", "<=1.2.3"));
        assert!(!satisfies("1.2.4", "<=1.2.3"));
        assert!(satisfies("1.2.4", ">1.2.3"));
        assert!(!satisfies("1.2.3", ">1.2.3"));
        assert!(satisfies("1.2.2", "<1.2.3"));
        assert!(!satisfies("1.2.3", "<1.2.3"));
    }
}
