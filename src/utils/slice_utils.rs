//! Generic slice and set utilities.
//! Ported from upstream utils_slice.go (104 lines).

use std::collections::HashMap;
use std::hash::Hash;

// ============================================================================
// GroupBy
// ============================================================================

/// Group elements by a key function.
/// Returns a map from key to vector of elements sharing that key.
pub fn group_by<T, K, F>(items: &[T], key_fn: F) -> HashMap<K, Vec<T>>
where
    K: Eq + Hash + Clone,
    T: Clone,
    F: Fn(&T) -> K,
{
    let mut result = HashMap::new();
    for item in items {
        result.entry(key_fn(item)).or_insert_with(Vec::new).push(item.clone());
    }
    result
}

// ============================================================================
// Array Utilities
// ============================================================================

/// Insert a separator between elements of a slice.
/// The separator function receives the 1-based index position between elements.
pub fn intersperse<T, F>(items: &[T], separator: F) -> Vec<T>
where
    T: Clone,
    F: Fn(usize) -> T,
{
    if items.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::with_capacity(items.len() * 2 - 1);
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            result.push(separator(i));
        }
        result.push(item.clone());
    }
    result
}

/// Return the number of elements that match the predicate.
pub fn count<T, F>(items: &[T], predicate: F) -> usize
where
    F: Fn(&T) -> bool,
{
    items.iter().filter(|item| predicate(item)).count()
}

/// Return a deduplicated slice, preserving first-occurrence order.
pub fn uniq<T>(items: &[T]) -> Vec<T>
where
    T: Eq + Hash + Clone,
{
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            result.push(item.clone());
        }
    }
    result
}

// ============================================================================
// Set Utilities (using HashSet)
// ============================================================================

/// Return elements in `a` that are not in `b`.
pub fn set_difference<T>(a: &std::collections::HashSet<T>, b: &std::collections::HashSet<T>) -> std::collections::HashSet<T>
where
    T: Eq + Hash + Clone,
{
    a.difference(b).cloned().collect()
}

/// Return true if `a` and `b` share any elements.
pub fn set_intersects<T>(a: &std::collections::HashSet<T>, b: &std::collections::HashSet<T>) -> bool
where
    T: Eq + Hash,
{
    a.intersection(b).next().is_some()
}

/// Return true if every element of `a` is also in `b` (a is subset of b).
pub fn set_every<T>(a: &std::collections::HashSet<T>, b: &std::collections::HashSet<T>) -> bool
where
    T: Eq + Hash,
{
    a.is_subset(b)
}

/// Return the union of `a` and `b`.
pub fn set_union<T>(a: &std::collections::HashSet<T>, b: &std::collections::HashSet<T>) -> std::collections::HashSet<T>
where
    T: Eq + Hash + Clone,
{
    a.union(b).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_by() {
        let items = vec!["apple", "banana", "avocado", "berry"];
        let grouped = group_by(&items, |s| s.chars().next().unwrap());
        assert_eq!(grouped[&'a'].len(), 2);
        assert_eq!(grouped[&'b'].len(), 2);
    }

    #[test]
    fn test_intersperse() {
        let items = vec![1, 2, 3];
        let result = intersperse(&items, |_| 0);
        assert_eq!(result, vec![1, 0, 2, 0, 3]);
    }

    #[test]
    fn test_intersperse_empty() {
        let items: Vec<i32> = vec![];
        let result = intersperse(&items, |_| 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_count() {
        let items = vec![1, 2, 3, 4, 5];
        assert_eq!(count(&items, |x| x % 2 == 0), 2);
    }

    #[test]
    fn test_uniq() {
        let items = vec![1, 2, 1, 3, 2, 1];
        assert_eq!(uniq(&items), vec![1, 2, 3]);
    }

    #[test]
    fn test_set_difference() {
        let mut a = std::collections::HashSet::new();
        a.insert(1);
        a.insert(2);
        a.insert(3);
        let mut b = std::collections::HashSet::new();
        b.insert(2);
        let diff = set_difference(&a, &b);
        assert!(diff.contains(&1));
        assert!(diff.contains(&3));
        assert!(!diff.contains(&2));
    }

    #[test]
    fn test_set_intersects() {
        let mut a = std::collections::HashSet::new();
        a.insert(1);
        a.insert(2);
        let mut b = std::collections::HashSet::new();
        b.insert(2);
        b.insert(3);
        assert!(set_intersects(&a, &b));

        let mut c = std::collections::HashSet::new();
        c.insert(4);
        assert!(!set_intersects(&a, &c));
    }

    #[test]
    fn test_set_every() {
        let mut a = std::collections::HashSet::new();
        a.insert(1);
        a.insert(2);
        let mut b = std::collections::HashSet::new();
        b.insert(1);
        b.insert(2);
        b.insert(3);
        assert!(set_every(&a, &b)); // a is subset of b

        let mut c = std::collections::HashSet::new();
        c.insert(4);
        assert!(!set_every(&a, &c));
    }

    #[test]
    fn test_set_union() {
        let mut a = std::collections::HashSet::new();
        a.insert(1);
        a.insert(2);
        let mut b = std::collections::HashSet::new();
        b.insert(2);
        b.insert(3);
        let union = set_union(&a, &b);
        assert_eq!(union.len(), 3);
        assert!(union.contains(&1));
        assert!(union.contains(&2));
        assert!(union.contains(&3));
    }
}