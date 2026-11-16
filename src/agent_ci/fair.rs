//! docs/specs/agent-ci.md section 3.1 (The slot): the order checks are started in. Repos take turns, so a repo
//! with a steady flow of pull requests can't keep every free permit from another's (2026-11-16: 67 checks started
//! for one repo and 5 for the other in two hours, because the first repo's keys were all tried before the second's).

/// `queues` (one per repo, each in its own order) dealt one at a time starting at queue `first`: queue `first`'s
/// first item, then the next queue's, and so on round, a queue dropping out when it is empty. Each item comes
/// with its queue's index. A repo with waiting work is reached within one round, so it gets a permit whenever at
/// least as many are free as repos wait; a lone queue is dealt whole.
pub fn deal<T>(queues: Vec<Vec<T>>, first: usize) -> Vec<(usize, T)> {
    let n = queues.len();
    let mut queues: Vec<_> = queues.into_iter().map(|q| q.into_iter()).collect();
    let mut out = Vec::new();
    let mut idle = 0;
    let mut at = if n == 0 { 0 } else { first % n };
    while idle < n {
        match queues[at].next() {
            Some(item) => {
                out.push((at, item));
                idle = 0;
            }
            None => idle += 1,
        }
        at = (at + 1) % n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(order: &[(usize, char)]) -> String {
        order.iter().map(|(_, c)| *c).collect()
    }

    #[test]
    fn two_repos_alternate() {
        let order = deal(vec![vec!['a', 'b', 'c', 'd'], vec!['w', 'x', 'y', 'z']], 0);
        assert_eq!(who(&order), "awbxcydz");
        let repos: Vec<_> = order.iter().map(|(i, _)| *i).collect();
        assert_eq!(repos, [0, 1, 0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn a_short_queue_gets_its_turn_in_the_first_round_and_then_drops_out() {
        let long: Vec<char> = "abcdefghij".chars().collect();
        assert_eq!(who(&deal(vec![long.clone(), vec!['w']], 0)), "awbcdefghij");
        assert_eq!(who(&deal(vec![long, vec!['w']], 1)), "wabcdefghij");
    }

    #[test]
    fn a_lone_queue_is_dealt_whole_and_empty_ones_are_skipped() {
        assert_eq!(who(&deal(vec![vec![], vec!['a', 'b', 'c'], vec![]], 0)), "abc");
        assert!(deal::<char>(vec![], 3).is_empty());
    }

    #[test]
    fn the_first_queue_rotates_with_the_pass() {
        let queues = || vec![vec!['a', 'b'], vec!['c', 'd'], vec!['e', 'f']];
        assert_eq!(who(&deal(queues(), 0)), "acebdf");
        assert_eq!(who(&deal(queues(), 1)), "ceadfb");
        assert_eq!(who(&deal(queues(), 2)), "eacfbd");
        assert_eq!(who(&deal(queues(), 3)), "acebdf");
    }
}
