//! Node handles never alias: 10,000 random mutations; a removed handle never resolves again
//! and tree links stay consistent.

use std::collections::HashSet;

use pageon_dom::{Dom, NodeData, NodeId};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[(self.next() as usize) % v.len()]
    }
}

fn check_links(dom: &Dom, live: &[NodeId]) {
    for &n in live {
        let kids: Vec<NodeId> = dom.children(n).collect();
        for (i, &k) in kids.iter().enumerate() {
            assert_eq!(dom.parent(k), Some(n));
            assert_eq!(
                dom.prev_sibling(k),
                if i == 0 { None } else { Some(kids[i - 1]) }
            );
        }
        assert_eq!(dom.last_child(n), kids.last().copied());
    }
}

#[test]
fn removed_handles_never_resolve() {
    let mut dom = Dom::new();
    let mut rng = Lcg(42);
    let mut live = vec![dom.root()];
    let mut dead: HashSet<NodeId> = HashSet::new();
    for step in 0..10_000 {
        match rng.next() % 3 {
            0 | 1 => {
                let parent = rng.pick(&live);
                let n = dom.create(NodeData::Text(format!("n{step}")));
                assert!(!dead.contains(&n), "a fresh handle equals a dead one");
                let kids: Vec<NodeId> = dom.children(parent).collect();
                if kids.is_empty() || rng.next().is_multiple_of(2) {
                    dom.append(parent, n).unwrap_or_else(|e| panic!("{e}"));
                } else {
                    let r = rng.pick(&kids);
                    dom.insert_before(parent, n, r)
                        .unwrap_or_else(|e| panic!("{e}"));
                }
                live.push(n);
            }
            _ => {
                if live.len() < 2 {
                    continue;
                }
                let victim = live[1 + (rng.next() as usize) % (live.len() - 1)];
                let subtree: Vec<NodeId> = dom
                    .traverse(victim)
                    .filter_map(|e| match e {
                        pageon_dom::Edge::Enter(id) => Some(id),
                        pageon_dom::Edge::Leave(_) => None,
                    })
                    .collect();
                dom.remove(victim).unwrap_or_else(|e| panic!("{e}"));
                for id in subtree {
                    dead.insert(id);
                }
                live.retain(|id| !dead.contains(id));
            }
        }
        for d in &dead {
            assert!(
                dom.get(*d).is_none(),
                "dead handle {d:?} resolved at step {step}"
            );
        }
        if step % 500 == 0 {
            check_links(&dom, &live);
        }
    }
    check_links(&dom, &live);
    assert_eq!(dom.len(), live.len());
}

#[test]
fn cycles_are_refused() {
    let mut dom = Dom::new();
    let a = dom.create(NodeData::DocumentFragment);
    let b = dom.create(NodeData::DocumentFragment);
    dom.append(dom.root(), a).unwrap_or_else(|e| panic!("{e}"));
    dom.append(a, b).unwrap_or_else(|e| panic!("{e}"));
    assert!(dom.append(b, a).is_err());
    assert!(dom.append(a, a).is_err());
}
