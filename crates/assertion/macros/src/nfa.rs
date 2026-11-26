//! A sequence's NFA, as `property.rs` builds it from `seq!`'s tree and
//! generates code from it.
//!
//! A state is a point within a cycle. Visiting it at an edge checks its
//! guards (the conditions of the `throughout`s around it), then follows its
//! transitions: within the cycle (a condition, a `let`, an epsilon), to the
//! next edge (a tick, setting the target's bit), into a delay's chain of
//! states, or to the match. Every loop passes through a tick or a chain, so
//! a visit ends.

use syn::{Expr, Ident};

use crate::seq::Count;

/// A condition as rewritten, and as written (for reports and spans).
#[derive(Clone)]
pub(crate) struct Cond {
    pub value: Expr,
    pub text: Expr,
}

/// What a branch does.
#[derive(Clone)]
pub(crate) enum Act {
    /// Visits a state at this edge.
    Visit(usize),
    /// Occupies a state from the next edge.
    Tick(usize),
    /// The branch dies at the condition (reported on the expectation side).
    Fail,
}

pub(crate) enum Trans {
    /// `if cond { then } else { otherwise }`.
    Branch {
        cond: Cond,
        then: Act,
        otherwise: Act,
    },
    /// Sets the lets struct's `field`, then visits `target`.
    Let {
        field: Ident,
        value: Expr,
        target: usize,
    },
    /// Visits `target` at this edge.
    Eps(usize),
    /// Occupies `target` from the next edge.
    Tick(usize),
    /// Enters chain `n`.
    Chain(usize),
    /// The sequence matched.
    Accept,
}

pub(crate) struct State {
    /// Conditions holding at every cycle a branch is here (`throughout`).
    pub guards: Vec<Cond>,
    pub trans: Vec<Trans>,
}

/// A delay: a run of states moved on by a shift, arriving at `exit`.
pub(crate) struct Chain {
    pub count: Count,
    pub guards: Vec<Cond>,
    pub exit: usize,
}

#[derive(Default)]
pub(crate) struct Nfa {
    pub states: Vec<State>,
    pub chains: Vec<Chain>,
    pub entry: usize,
}

impl Nfa {
    /// The states occupied across an edge (ticks' targets), each with its
    /// bit after the chains', the chains' first bits, and the bits in all.
    pub fn bits(&self) -> (Vec<(usize, usize)>, Vec<usize>, usize) {
        let mut next = 0;
        let chain_lo: Vec<usize> = self
            .chains
            .iter()
            .map(|c| {
                let lo = next;
                next += c.count.delay_states();
                lo
            })
            .collect();
        let mut ticked: Vec<usize> = Vec::new();
        for state in &self.states {
            for t in &state.trans {
                let targets: Vec<usize> = match t {
                    Trans::Branch {
                        then, otherwise, ..
                    } => [then, otherwise]
                        .into_iter()
                        .filter_map(|a| match a {
                            Act::Tick(s) => Some(*s),
                            _ => None,
                        })
                        .collect(),
                    Trans::Tick(s) => vec![*s],
                    _ => Vec::new(),
                };
                for s in targets {
                    if !ticked.contains(&s) {
                        ticked.push(s);
                    }
                }
            }
        }
        ticked.sort_unstable();
        let bits = ticked
            .into_iter()
            .map(|s| {
                let b = next;
                next += 1;
                (s, b)
            })
            .collect();
        (bits, chain_lo, next)
    }
}
