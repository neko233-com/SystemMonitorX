//! Conservative, stable process-tree projection of an already sorted visible set.
//! Parent identity is checked using creation time; no Windows APIs are called.
use std::collections::{HashMap, HashSet, hash_map::Entry};

use crate::monitor::ProcessSample;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub index: usize,
    pub depth: usize,
    pub has_children: bool,
}

fn parent_created_first(parent: &ProcessSample, child: &ProcessSample) -> bool {
    match (
        parent.start_time_windows_ticks.filter(|ticks| *ticks > 0),
        child.start_time_windows_ticks.filter(|ticks| *ticks > 0),
    ) {
        (Some(parent), Some(child)) => parent <= child,
        // Different known Unix seconds establish order even without full ticks.
        // Equal seconds cannot rule out PID reuse, so never infer that edge.
        _ => {
            parent.start_time_unix_secs > 0
                && child.start_time_unix_secs > 0
                && parent.start_time_unix_secs < child.start_time_unix_secs
        }
    }
}

/// Roots and siblings follow the input sort order. Invalid/duplicate indices are
/// ignored. A collapsed node is retained, with its descendants omitted; only an
/// exact positive FILETIME identity can collapse it. All other visible nodes are
/// emitted exactly once. Work and memory are linear in the visible input size.
pub fn flatten(
    processes: &[ProcessSample],
    ordered_indices: &[usize],
    collapsed: &HashSet<(u32, u64)>,
) -> Vec<TreeRow> {
    let mut seen = HashSet::with_capacity(ordered_indices.len());
    let indices: Vec<usize> = ordered_indices
        .iter()
        .copied()
        .filter(|index| *index < processes.len() && seen.insert(*index))
        .collect();
    let count = indices.len();
    let mut by_pid: HashMap<u32, Option<usize>> = HashMap::with_capacity(count);
    for (slot, index) in indices.iter().enumerate() {
        match by_pid.entry(processes[*index].pid) {
            Entry::Vacant(entry) => {
                entry.insert(Some(slot));
            }
            Entry::Occupied(mut entry) => {
                // Ambiguous duplicate PIDs cannot identify anyone's parent.
                entry.insert(None);
            }
        }
    }
    let mut parents: Vec<Option<usize>> = indices
        .iter()
        .map(|index| {
            let child = &processes[*index];
            let parent_pid = child.parent_pid?;
            if parent_pid == child.pid {
                return None;
            }
            let parent_slot = by_pid.get(&parent_pid).copied().flatten()?;
            parent_created_first(&processes[indices[parent_slot]], child).then_some(parent_slot)
        })
        .collect();

    // Each node has at most one parent. Three-colour walks find and cut cycles
    // without recursive calls or repeatedly walking previously completed paths.
    let mut colour = vec![0u8; count];
    let mut path = Vec::new();
    for start in 0..count {
        if colour[start] != 0 {
            continue;
        }
        path.clear();
        let mut cursor = Some(start);
        while let Some(slot) = cursor {
            match colour[slot] {
                0 => {
                    colour[slot] = 1;
                    path.push(slot);
                    cursor = parents[slot];
                }
                1 => {
                    // Deterministically turn the first encountered cycle node
                    // into a root, leaving the remaining safe edges intact.
                    parents[slot] = None;
                    break;
                }
                _ => break,
            }
        }
        for slot in &path {
            colour[*slot] = 2;
        }
    }

    let mut children = vec![Vec::<usize>::new(); count];
    let mut roots = Vec::new();
    for (slot, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(slot);
        } else {
            roots.push(slot);
        }
    }
    let mut pending: Vec<(usize, usize)> = roots.into_iter().rev().map(|slot| (slot, 0)).collect();
    let mut rows = Vec::with_capacity(count);
    while let Some((slot, depth)) = pending.pop() {
        let index = indices[slot];
        let process = &processes[index];
        rows.push(TreeRow {
            index,
            depth,
            has_children: !children[slot].is_empty(),
        });
        if process
            .start_time_windows_ticks
            .filter(|ticks| *ticks > 0)
            .is_some_and(|ticks| collapsed.contains(&(process.pid, ticks)))
        {
            continue;
        }
        pending.extend(children[slot].iter().rev().map(|child| (*child, depth + 1)));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{TreeRow, flatten};
    use crate::monitor::ProcessSample;
    use std::collections::HashSet;

    fn process(pid: u32, parent: Option<u32>, ticks: u64) -> ProcessSample {
        ProcessSample {
            pid,
            parent_pid: parent,
            start_time_unix_secs: 1_700_000_000,
            start_time_windows_ticks: Some(ticks),
            ..Default::default()
        }
    }

    #[test]
    fn roots_and_siblings_keep_sorted_input_order() {
        let processes = vec![
            process(10, None, 10),
            process(20, None, 10),
            process(11, Some(10), 11),
            process(12, Some(10), 12),
            process(21, Some(20), 11),
        ];
        let rows = flatten(&processes, &[3, 1, 4, 2, 0], &HashSet::new());
        assert_eq!(
            rows,
            vec![
                TreeRow {
                    index: 1,
                    depth: 0,
                    has_children: true
                },
                TreeRow {
                    index: 4,
                    depth: 1,
                    has_children: false
                },
                TreeRow {
                    index: 0,
                    depth: 0,
                    has_children: true
                },
                TreeRow {
                    index: 3,
                    depth: 1,
                    has_children: false
                },
                TreeRow {
                    index: 2,
                    depth: 1,
                    has_children: false
                },
            ]
        );
        // Filtering out a parent must not manufacture a hidden root row.
        assert_eq!(
            flatten(&processes, &[2], &HashSet::new()),
            vec![TreeRow {
                index: 2,
                depth: 0,
                has_children: false
            }]
        );
    }

    #[test]
    fn reused_parent_pid_does_not_capture_an_older_child() {
        let processes = vec![process(10, None, 300), process(11, Some(10), 200)];
        assert_eq!(
            flatten(&processes, &[0, 1], &HashSet::new())
                .iter()
                .map(|row| row.depth)
                .collect::<Vec<_>>(),
            vec![0, 0]
        );
    }

    #[test]
    fn full_ticks_resolve_same_second_order_and_unknown_time_is_conservative() {
        let mut processes = vec![
            process(10, None, 101),
            process(11, Some(10), 100),
            process(12, Some(10), 102),
        ];
        let rows = flatten(&processes, &[0, 1, 2], &HashSet::new());
        assert_eq!(
            rows.iter()
                .map(|row| (row.index, row.depth))
                .collect::<Vec<_>>(),
            vec![(0, 0), (2, 1), (1, 0)]
        );
        processes[0].start_time_windows_ticks = None;
        assert!(
            flatten(&processes, &[0, 1, 2], &HashSet::new())
                .iter()
                .all(|row| row.depth == 0)
        );
        processes[2].start_time_unix_secs += 1;
        let rows = flatten(&processes, &[0, 2], &HashSet::new());
        assert_eq!(
            rows.iter().map(|row| row.depth).collect::<Vec<_>>(),
            vec![0, 1]
        );
        processes[0].start_time_unix_secs = 0;
        assert!(
            flatten(&processes, &[0, 2], &HashSet::new())
                .iter()
                .all(|row| row.depth == 0)
        );
    }

    #[test]
    fn cycles_are_cut_and_every_node_appears_once() {
        let processes = vec![
            process(10, Some(20), 100),
            process(20, Some(30), 100),
            process(30, Some(10), 100),
            process(40, Some(10), 101),
        ];
        let rows = flatten(&processes, &[3, 1, 0, 2], &HashSet::new());
        assert_eq!(rows.len(), processes.len());
        let mut indices: Vec<_> = rows.iter().map(|row| row.index).collect();
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1, 2, 3]);
        assert_eq!(rows.iter().filter(|row| row.depth == 0).count(), 1);
        assert!(rows.iter().all(|row| row.depth < processes.len()));
    }

    #[test]
    fn collapse_requires_exact_creation_ticks_and_hides_descendants() {
        let processes = vec![
            process(10, None, 100),
            process(11, Some(10), 101),
            process(12, Some(11), 102),
            process(20, None, 100),
        ];
        let collapsed = HashSet::from([(10, 100)]);
        assert_eq!(
            flatten(&processes, &[0, 1, 2, 3], &collapsed),
            vec![
                TreeRow {
                    index: 0,
                    depth: 0,
                    has_children: true
                },
                TreeRow {
                    index: 3,
                    depth: 0,
                    has_children: false
                }
            ]
        );
        assert_eq!(
            flatten(&processes, &[0, 1, 2, 3], &HashSet::from([(10, 99)])).len(),
            4
        );
        let mut unknown = processes;
        unknown[0].start_time_windows_ticks = None;
        assert_eq!(
            flatten(&unknown, &[0, 1, 2, 3], &HashSet::from([(10, 0)])).len(),
            4
        );
    }

    #[test]
    fn duplicate_indices_ambiguous_pids_and_self_edges_are_safe() {
        let processes = vec![
            process(10, Some(10), 100),
            process(11, Some(10), 101),
            process(10, None, 100),
        ];
        let rows = flatten(&processes, &[2, usize::MAX, 1, 2, 0], &HashSet::new());
        assert_eq!(
            rows.iter().map(|row| row.index).collect::<Vec<_>>(),
            vec![2, 1, 0]
        );
        assert!(rows.iter().all(|row| row.depth == 0 && !row.has_children));
    }

    #[test]
    fn ten_thousand_node_chain_uses_no_recursive_stack() {
        let count = 10_000usize;
        let processes: Vec<_> = (0..count)
            .map(|index| {
                process(
                    index as u32 + 1,
                    (index > 0).then_some(index as u32),
                    index as u64 + 100,
                )
            })
            .collect();
        let ordered: Vec<_> = (0..count).rev().collect();
        let rows = flatten(&processes, &ordered, &HashSet::new());
        assert_eq!(rows.len(), count);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(
                (row.index, row.depth, row.has_children),
                (index, index, index + 1 < count)
            );
        }
    }
}
