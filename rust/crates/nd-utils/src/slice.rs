//! Generic slice helpers — port of `utils/slice`. Rust-stdlib idioms replace
//! Go-iterator adapters; Go channels become `Iterator` where relevant.

use std::collections::HashMap;
use std::hash::Hash;

/// Maps each element of `t` through `map_func` (`slice.Map`).
#[must_use]
pub fn map<T, R>(t: &[T], map_func: impl Fn(&T) -> R) -> Vec<R> {
    t.iter().map(map_func).collect()
}

/// Maps each element of `t` with the extra argument `arg`
/// (`slice.MapWithArg`).
#[must_use]
pub fn map_with_arg<I, O, A: Clone>(t: &[I], arg: A, map_func: impl Fn(A, &I) -> O) -> Vec<O> {
    t.iter().map(|e| map_func(arg.clone(), e)).collect()
}

/// Groups `s` by the result of `key_func` (`slice.Group`).
#[must_use]
pub fn group<T, K: Eq + Hash + Clone>(s: &[T], key_func: impl Fn(&T) -> K) -> HashMap<K, Vec<&T>> {
    let mut m: HashMap<K, Vec<&T>> = HashMap::new();
    for item in s {
        m.entry(key_func(item)).or_default().push(item);
    }
    m
}

/// Groups `s` by `key_func` with the groups in first-seen order
/// (`slice.GroupOrdered`).
#[must_use]
pub fn group_ordered<T, K: Eq + Hash + Clone>(s: &[T], key_func: impl Fn(&T) -> K) -> Vec<Vec<&T>> {
    let mut groups: Vec<Vec<&T>> = Vec::new();
    let mut index: HashMap<K, usize> = HashMap::new();
    for item in s {
        let k = key_func(item);
        let i = *index.entry(k).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[i].push(item);
    }
    groups
}

/// Builds a map from a slice using `transform_func` yielding (key, value) pairs
/// (`slice.ToMap`).
#[must_use]
pub fn to_map<T, K: Eq + Hash, V>(s: &[T], transform_func: impl Fn(&T) -> (K, V)) -> HashMap<K, V> {
    s.iter().map(transform_func).collect()
}

/// Builds a set (a `HashSet`) for O(1) membership tests. Duplicate elements
/// collapse to a single key (`slice.ToSet`).
#[must_use]
pub fn to_set<T: Eq + Hash + Clone>(s: &[T]) -> std::collections::HashSet<T> {
    s.iter().cloned().collect()
}

/// Returns the distinct elements of `list`, ordered by descending frequency
/// (`slice.CompactByFrequency`). Ties keep the ascending-key order of Go's
/// `slices.SortedFunc`.
#[must_use]
pub fn compact_by_frequency<T: Eq + Hash + Ord + Clone>(list: &[T]) -> Vec<T> {
    let mut counters: HashMap<T, usize> = HashMap::new();
    for item in list {
        *counters.entry(item.clone()).or_default() += 1;
    }
    let mut keys: Vec<T> = counters.keys().cloned().collect();
    keys.sort_by(|i, j| {
        let (ci, cj) = (counters[j], counters[i]);
        ci.cmp(&cj).then_with(|| i.cmp(j))
    });
    keys
}

/// Returns the item that appeared the most times, ignoring zero values
/// (`slice.MostFrequent`). Returns `None` for an empty list or a list of only
/// zero values.
#[must_use]
pub fn most_frequent<T: Eq + Hash + Clone + Default>(list: &[T]) -> Option<T> {
    if list.is_empty() {
        return None;
    }
    let zero = T::default();
    let mut counters: HashMap<T, usize> = HashMap::new();
    let mut top_item: Option<T> = None;
    let mut top_count = 0usize;

    for v in list {
        if *v == zero {
            continue;
        }
        let value = v.clone();
        let slot = counters.entry(value.clone()).or_insert(0);
        *slot += 1;
        let c = *slot;
        if c > top_count {
            top_item = Some(value);
            top_count = c;
        }
    }
    top_item
}

/// Inserts `value` into `slice` at `index`, shifting the tail right
/// (`slice.Insert`). Panics when out of bounds, matching Go's slice semantics.
#[must_use]
pub fn insert<T: Clone>(slice: &[T], value: T, index: usize) -> Vec<T> {
    let mut r = slice.to_vec();
    r.insert(index, value);
    r
}

/// Removes the element at `index` (`slice.Remove`).
#[must_use]
pub fn remove<T: Clone>(slice: &[T], index: usize) -> Vec<T> {
    let mut r = slice.to_vec();
    r.remove(index);
    r
}

/// Moves the element at `src_index` to `dst_index` (`slice.Move`).
#[must_use]
pub fn mv<T: Clone>(slice: &[T], src_index: usize, dst_index: usize) -> Vec<T> {
    let value = slice[src_index].clone();
    let without = remove(slice, src_index);
    insert(&without, value, dst_index)
}

/// Returns the unique elements of `list` in first-seen order (`slice.Unique`).
#[must_use]
pub fn unique<T: Eq + Hash + Clone>(list: &[T]) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for item in list {
        if seen.insert(item.clone()) {
            result.push(item.clone());
        }
    }
    result
}

/// Returns a vector of lines read from `data`, following Go's `scanLines`
/// splitting: lines terminated by `\n`, `\r`, `\r\n`, or EOF (`slice.LinesFrom`).
#[must_use]
pub fn lines_from(data: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let b = data.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'\n' {
            lines.push(data[start..i].to_string());
            i += 1;
            start = i;
        } else if b[i] == b'\r' {
            lines.push(data[start..i].to_string());
            i += if i + 1 < b.len() && b[i + 1] == b'\n' {
                2
            } else {
                1
            };
            start = i;
        } else {
            i += 1;
        }
    }
    if start < b.len() {
        lines.push(data[start..].to_string());
    }
    lines
}

/// Collects chunks of `n` elements from the input iterator
/// (`slice.CollectChunks`). A trailing partial chunk is emitted, matching Go.
pub fn collect_chunks<I: Iterator>(it: I, n: usize) -> Vec<Vec<I::Item>> {
    let mut result = Vec::new();
    let mut s = Vec::with_capacity(n);
    for x in it {
        s.push(x);
        if s.len() >= n {
            result.push(std::mem::take(&mut s));
        }
    }
    if !s.is_empty() {
        result.push(s);
    }
    result
}

/// Returns the elements of `s` for which `filter_func` returns true
/// (`slice.Filter`).
#[must_use]
pub fn filter<T: Clone>(s: &[T], filter_func: impl Fn(&T) -> bool) -> Vec<T> {
    s.iter().filter(|i| filter_func(i)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn map_returns_empty_slice_for_an_empty_input() {
        let result: Vec<String> = map(&[], |v: &i32| (v * 2).to_string());
        assert!(result.is_empty());
    }

    #[test]
    fn map_returns_a_new_slice_with_elements_mapped() {
        let result = map(&[1, 2, 3, 4], |v| (v * 2).to_string());
        assert_eq!(result, ["2", "4", "6", "8"]);
    }

    #[test]
    fn map_with_arg_returns_empty_slice_for_an_empty_input() {
        let result: Vec<String> = map_with_arg(&[], 10, |a: i32, v: &i32| (a + v).to_string());
        assert!(result.is_empty());
    }

    #[test]
    fn map_with_arg_maps_with_the_argument() {
        let result = map_with_arg(&[1, 2, 3, 4], 10, |a, v: &i32| (a + v).to_string());
        assert_eq!(result, ["11", "12", "13", "14"]);
    }

    #[test]
    fn group_returns_empty_map_for_an_empty_input() {
        let result: HashMap<i32, Vec<&i32>> = group(&[], |v: &i32| v % 2);
        assert!(result.is_empty());
    }

    #[test]
    fn group_groups_by_the_key_function() {
        let input = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let result = group(&input, |v| v % 2);
        assert_eq!(result.len(), 2);
        let mut evens: Vec<i32> = result[&0].iter().map(|v| **v).collect();
        let mut odds: Vec<i32> = result[&1].iter().map(|v| **v).collect();
        evens.sort();
        odds.sort();
        assert_eq!(evens, [2, 4, 6, 8, 10]);
        assert_eq!(odds, [1, 3, 5, 7, 9, 11]);
    }

    #[test]
    fn group_ordered_keeps_first_seen_order() {
        let input = [2, 1, 4, 3, 5, 6, 8];
        let result = group_ordered(&input, |v| v % 3);
        let got: Vec<Vec<i32>> = result
            .iter()
            .map(|g| g.iter().map(|v| **v).collect())
            .collect();
        assert_eq!(got, vec![vec![2, 5, 8], vec![1, 4], vec![3, 6]]);
        // Empty input yields no groups (Go returns nil).
        assert!(group_ordered::<i32, i32>(&[], |v| *v).is_empty());
    }

    #[test]
    fn to_map_returns_the_transformed_pairs() {
        let result = to_map(&[1, 2, 3, 4], |v| (v * 2, (v * 2).to_string()));
        assert_eq!(result.len(), 4);
        assert_eq!(result[&2], "2");
        assert_eq!(result[&4], "4");
        assert_eq!(result[&6], "6");
        assert_eq!(result[&8], "8");
    }

    #[test]
    fn to_set_builds_one_key_per_distinct_element() {
        let result = to_set(&[1, 2, 2, 3, 3, 3]);
        assert_eq!(result.len(), 3);
        for k in [1, 2, 3] {
            assert!(result.contains(&k));
        }
    }

    #[test]
    fn compact_by_frequency_groups_by_frequency() {
        let result = compact_by_frequency(&[1, 2, 1, 2, 3, 2]);
        assert_eq!(result, [2, 1, 3]);
        assert!(compact_by_frequency::<i32>(&[]).is_empty());
    }

    #[test]
    fn most_frequent_returns_the_mode_ignoring_zeroes() {
        assert_eq!(most_frequent(&[]), None::<i32>);
        assert_eq!(most_frequent(&["123"]), Some("123"));
        assert_eq!(most_frequent(&["1", "2", "1", "2", "3", "2"]), Some("2"));
        assert_eq!(most_frequent(&[0, 0, 0, 2, 2]), Some(2));
    }

    #[test]
    fn move_reorders_elements() {
        assert_eq!(mv(&["1", "2", "3"], 0, 2), ["2", "3", "1"]);
        assert_eq!(mv(&["1", "2", "3"], 2, 0), ["3", "1", "2"]);
        assert_eq!(mv(&["1", "2", "3"], 1, 1), ["1", "2", "3"]);
    }

    #[test]
    fn unique_returns_first_seen_unique_elements() {
        assert!(unique::<i32>(&[]).is_empty());
        assert_eq!(unique(&[1, 2, 1, 2, 3, 2]), [1, 2, 3]);
    }

    #[test]
    fn lines_from_splits_like_go_scanlines() {
        // Empty input: no lines.
        assert!(lines_from("").is_empty());
        // "test.mp3\ntest.ogg" → 2 lines.
        assert_eq!(
            lines_from("test.mp3\ntest.ogg"),
            ["test.mp3".to_string(), "test.ogg".to_string()]
        );
        // Non-terminated final line is returned.
        assert_eq!(lines_from("abc"), ["abc".to_string()]);
        assert_eq!(lines_from("abc\n"), ["abc".to_string()]);
        // \r\n and lone \r both terminate a line.
        assert_eq!(
            lines_from("a\r\nb\rc"),
            ["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn collect_chunks_breaks_up_the_iterator() {
        assert!(collect_chunks(std::iter::empty::<i32>(), 1).is_empty());
        assert_eq!(
            collect_chunks([1, 2, 3].into_iter(), 10),
            vec![vec![1, 2, 3]]
        );
        assert_eq!(
            collect_chunks([1, 2, 3, 4, 5].into_iter(), 3),
            vec![vec![1, 2, 3], vec![4, 5]]
        );
    }

    #[test]
    fn filter_keeps_only_matching_elements_in_order() {
        assert!(filter(&[], |v: &i32| *v > 0).is_empty());
        assert_eq!(filter(&[1, 2, 3, 4], |v| *v > 0), [1, 2, 3, 4]);
        assert!(filter(&[1, 2, 3, 4], |v| *v > 10).is_empty());
        assert_eq!(filter(&[1, 2, 3, 4, 5, 6], |v| v % 2 == 0), [2, 4, 6]);
        assert_eq!(
            filter(&["a", "abc", "abcd", "ab", "abcde"], |s| s.len() > 3),
            ["abcd", "abcde"]
        );
        assert_eq!(
            filter(&[9, 8, 7, 6, 5, 4, 3, 2, 1], |v| v % 2 == 1),
            [9, 7, 5, 3, 1]
        );
    }
}
