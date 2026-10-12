//! One deterministic case-insensitive name rule shared by process/device tables.
//! Names use Unicode lowercase lexicographic order, without locale-dependent
//! collation, basename guessing, extension stripping or simulated sort keys.

use std::{cmp::Ordering, collections::HashMap};

pub fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// Compare with the same string-level lowercase rule as name_key. ASCII
/// needs no allocation; Unicode uses contextual mappings such as final sigma.
pub fn compare_names(left: &str, right: &str) -> Ordering {
    if left.is_ascii() && right.is_ascii() {
        left.bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
    } else {
        name_key(left).cmp(&name_key(right))
    }
}

#[derive(Clone, Copy)]
pub struct ProcessNameInput<'a> {
    pub name: &'a str,
    pub pid: u32,
    pub creation_time_windows_ticks: Option<u64>,
    pub start_time_unix_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RowIdentity {
    pid: u32,
    creation_ticks: Option<u64>,
    fallback_birth_seconds: u64,
}
impl RowIdentity {
    fn of(input: &ProcessNameInput<'_>) -> Self {
        let creation_ticks = input.creation_time_windows_ticks.filter(|ticks| *ticks > 0);
        Self {
            pid: input.pid,
            creation_ticks,
            fallback_birth_seconds: if creation_ticks.is_none() {
                input.start_time_unix_secs
            } else {
                0
            },
        }
    }
    fn birth_order(self) -> u64 {
        self.creation_ticks.unwrap_or(self.fallback_birth_seconds)
    }
}

#[derive(Debug)]
pub struct NameOrderResult {
    pub indices: Vec<usize>,
    pub reused: bool,
}

/// Caches display ordering only, never mutation identity or old metrics.
/// A fresh snapshot with unchanged case-insensitive names and process birth
/// identities remaps indices in O(n) without sorting or cloning old samples.
#[derive(Default)]
pub struct NameOrderCache {
    names: HashMap<RowIdentity, String>,
    ordered: Vec<RowIdentity>,
    sort_count: u64,
}
impl NameOrderCache {
    pub fn sort_count(&self) -> u64 {
        self.sort_count
    }

    pub fn order(&mut self, inputs: &[ProcessNameInput<'_>]) -> NameOrderResult {
        let mut incoming = HashMap::with_capacity(inputs.len());
        let mut unique = true;
        let mut unchanged = inputs.len() == self.names.len();
        for (index, input) in inputs.iter().enumerate() {
            let identity = RowIdentity::of(input);
            unique &= incoming.insert(identity, index).is_none();
            unchanged &= self
                .names
                .get(&identity)
                .is_some_and(|name| compare_names(name, input.name) == Ordering::Equal);
        }
        if unique && unchanged && self.ordered.len() == inputs.len() {
            return NameOrderResult {
                indices: self
                    .ordered
                    .iter()
                    .map(|identity| incoming[identity])
                    .collect(),
                reused: true,
            };
        }
        let mut keyed: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| (RowIdentity::of(input), name_key(input.name), index))
            .collect();
        keyed.sort_unstable_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then(left.0.pid.cmp(&right.0.pid))
                .then(left.0.birth_order().cmp(&right.0.birth_order()))
                .then(left.2.cmp(&right.2))
        });
        self.sort_count = self.sort_count.saturating_add(1);
        self.names.clear();
        self.ordered.clear();
        let indices = keyed.iter().map(|(_, _, index)| *index).collect();
        if unique {
            for (identity, name, _) in keyed {
                self.names.insert(identity, name);
                self.ordered.push(identity);
            }
        }
        NameOrderResult {
            indices,
            reused: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_share_unicode_case_insensitive_prefix_order() {
        let names = [
            "zeta.exe",
            "ALPHA.exe",
            "alpha-child.exe",
            "Beta.exe",
            "éclair.exe",
            "ÉCLAIR.exe",
        ];
        let mut by_key = names;
        by_key.sort_by_key(|name| name_key(name));
        let mut by_comparison = names;
        by_comparison.sort_by(|left, right| compare_names(left, right));
        assert_eq!(by_key, by_comparison);
        assert_eq!(
            &by_key[..4],
            &["alpha-child.exe", "ALPHA.exe", "Beta.exe", "zeta.exe"]
        );
        assert_eq!(compare_names("ÉCLAIR.exe", "éclair.exe"), Ordering::Equal);
        assert_ne!(compare_names("a.exe", "a-child.exe"), Ordering::Equal);
    }

    #[test]
    fn ascii_comparison_matches_keys_without_changing_prefix_or_punctuation() {
        for left in ["", "A", "alpha.exe", "ALPHA-child.exe", " space", "z", "_"] {
            for right in ["", "a", "ALPHA.exe", "alpha-child.exe", " space", "Z", "_"] {
                assert_eq!(
                    compare_names(left, right),
                    name_key(left).cmp(&name_key(right))
                );
            }
        }
    }

    #[test]
    fn contextual_unicode_comparison_matches_string_keys() {
        assert_eq!(name_key("ΟΣ"), "ος");
        assert_eq!(compare_names("ΟΣ", "ος"), Ordering::Equal);
        let names = [
            "ΟΣ", "ος", "Οσ", "ΟΤ", "ΑΣ", "ας", "ÉCLAIR", "İ", "i\u{307}", "ascii",
        ];
        for left in names {
            for right in names {
                assert_eq!(
                    compare_names(left, right),
                    name_key(left).cmp(&name_key(right))
                );
            }
        }
    }

    #[test]
    fn final_sigma_names_keep_equal_key_pid_order_and_reuse_cache() {
        let mut cache = NameOrderCache::default();
        let make = |name, pid, ticks| ProcessNameInput {
            name,
            pid,
            creation_time_windows_ticks: Some(ticks),
            start_time_unix_secs: 1,
        };
        let inputs = [make("ΟΣ", 2, 20), make("ος", 1, 10), make("ΟΤ", 3, 30)];
        assert_eq!(cache.order(&inputs).indices, vec![1, 0, 2]);
        assert_eq!(cache.sort_count(), 1);
        let reordered = [inputs[2], make("ος", 2, 20), make("ΟΣ", 1, 10)];
        let result = cache.order(&reordered);
        assert!(result.reused);
        assert_eq!(result.indices, vec![2, 1, 0]);
        assert_eq!(cache.sort_count(), 1);
    }

    #[test]
    fn order_reuses_real_names_after_reordering_and_rejects_birth_reuse() {
        let mut cache = NameOrderCache::default();
        let make = |name, pid, ticks| ProcessNameInput {
            name,
            pid,
            creation_time_windows_ticks: Some(ticks),
            start_time_unix_secs: 1,
        };
        let inputs = [
            make("svchost.exe", 42, 100),
            make("Explorer.EXE", 10, 50),
            make("svchost.exe", 41, 200),
            make("SystemMonitorX.exe", 91, 300),
        ];
        assert_eq!(cache.order(&inputs).indices, vec![1, 2, 0, 3]);
        assert_eq!(cache.sort_count(), 1);
        let reordered = [
            inputs[3],
            inputs[0],
            inputs[2],
            make("EXPLORER.exe", 10, 50),
        ];
        let reused = cache.order(&reordered);
        assert!(reused.reused);
        assert_eq!(reused.indices, vec![3, 2, 1, 0]);
        assert_eq!(cache.sort_count(), 1);
        let mut recycled = reordered;
        recycled[1].creation_time_windows_ticks = Some(101);
        assert!(!cache.order(&recycled).reused);
        assert_eq!(cache.sort_count(), 2);
        recycled[1].name = "aardvark.exe";
        assert_eq!(cache.order(&recycled).indices[0], 1);
        assert_eq!(cache.sort_count(), 3);
    }

    #[test]
    fn equal_names_tie_by_pid_and_exact_creation_not_case() {
        let mut cache = NameOrderCache::default();
        let inputs = [
            ProcessNameInput {
                name: "ALPHA",
                pid: 3,
                creation_time_windows_ticks: Some(30),
                start_time_unix_secs: 1,
            },
            ProcessNameInput {
                name: "alpha",
                pid: 2,
                creation_time_windows_ticks: Some(21),
                start_time_unix_secs: 1,
            },
            ProcessNameInput {
                name: "Alpha",
                pid: 2,
                creation_time_windows_ticks: Some(20),
                start_time_unix_secs: 1,
            },
        ];
        assert_eq!(cache.order(&inputs).indices, vec![2, 1, 0]);
    }

    #[test]
    fn ambiguous_duplicate_identities_never_reuse_order() {
        let mut cache = NameOrderCache::default();
        let input = ProcessNameInput {
            name: "app.exe",
            pid: 1,
            creation_time_windows_ticks: Some(1),
            start_time_unix_secs: 0,
        };
        assert!(!cache.order(&[input, input]).reused);
        assert!(!cache.order(&[input, input]).reused);
    }
}
