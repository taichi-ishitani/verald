//! The run-time parts of temporal properties: the states of a sequence's
//! NFA as a bit set, the threads and attempts stepped at each edge, and the
//! samples of earlier edges `past` reads. `#[assertions]` generates, per
//! sequence, the code that evaluates its elements and moves its bits; what
//! is here keeps the threads, merges them and reports.

use crate::{AssertionSignals, Failure};

/// The NFA states a thread occupies: one bit per state, `W` words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct States<const W: usize>([u64; W]);

impl<const W: usize> Default for States<W> {
    fn default() -> Self {
        States([0; W])
    }
}

impl<const W: usize> States<W> {
    pub const EMPTY: Self = States([0; W]);

    /// The states `lo..=hi` (none if `lo > hi`), a word at a time.
    pub const fn range(lo: usize, hi: usize) -> Self {
        let mut words = [0; W];
        if lo > hi {
            return States(words);
        }
        let mut w = lo / 64;
        while w <= hi / 64 && w < W {
            let start = if w == lo / 64 { lo % 64 } else { 0 };
            let end = if w == hi / 64 { hi % 64 } else { 63 };
            let width = end - start + 1;
            words[w] = if width == 64 {
                u64::MAX
            } else {
                ((1 << width) - 1) << start
            };
            w += 1;
        }
        States(words)
    }

    #[inline]
    pub fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 != 0
    }

    #[inline]
    pub fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|w| *w == 0)
    }

    #[inline]
    pub fn intersects(&self, other: &Self) -> bool {
        self.0.iter().zip(&other.0).any(|(a, b)| a & b != 0)
    }

    #[inline]
    fn or(&mut self, other: &Self) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= b;
        }
    }

    /// The states `lo..=hi` one edge later: each moves to the next, the
    /// last one leaving the range, or staying when `sticky` (an unbounded
    /// delay's last state). A shift of the words by one bit, carried from
    /// each word into the next, so the cost does not grow with the range.
    #[inline]
    pub fn advance(&self, lo: usize, hi: usize, sticky: bool) -> Self {
        let within = Self::range(lo, hi);
        let mut next = Self::EMPTY;
        let mut carry = 0;
        for i in 0..W {
            let word = self.0[i] & within.0[i];
            next.0[i] = word << 1 | carry;
            carry = word >> 63;
        }
        // What left the range (`hi + 1`) is dropped.
        let kept = Self::range(lo + 1, hi);
        for i in 0..W {
            next.0[i] &= kept.0[i];
        }
        if sticky && self.get(hi) {
            next.set(hi);
        }
        next
    }
}

/// What one edge of a sequence gives: the states its branches occupy
/// next, per value of the `let`s (a `let` taken splits a branch off), the
/// matches, and the last element that failed.
pub struct Out<L, const W: usize> {
    pieces: Vec<(States<W>, L)>,
    matches: Vec<L>,
    failure: Option<Failure>,
}

impl<L, const W: usize> Default for Out<L, W> {
    fn default() -> Self {
        Out {
            pieces: Vec::new(),
            matches: Vec::new(),
            failure: None,
        }
    }
}

impl<L: Clone + PartialEq, const W: usize> Out<L, W> {
    fn clear(&mut self) {
        self.pieces.clear();
        self.matches.clear();
        self.failure = None;
    }

    fn piece(&mut self, lets: &L) -> &mut States<W> {
        let i = match self.pieces.iter().position(|(_, l)| l == lets) {
            Some(i) => i,
            None => {
                self.pieces.push((States::EMPTY, lets.clone()));
                self.pieces.len() - 1
            }
        };
        &mut self.pieces[i].0
    }

    /// A branch with `lets` occupies the state `i` next.
    #[inline]
    pub fn set(&mut self, lets: &L, i: usize) {
        self.piece(lets).set(i);
    }

    /// Branches with `lets` occupy `states` next.
    #[inline]
    pub fn or(&mut self, lets: &L, states: &States<W>) {
        if !states.is_empty() {
            self.piece(lets).or(states);
        }
    }

    /// A branch with `lets` matched at this edge. Branches reaching the
    /// match by several paths count once.
    #[inline]
    pub fn matched(&mut self, lets: &L) {
        if !self.matches.contains(lets) {
            self.matches.push(lets.clone());
        }
    }

    /// A branch died at the element `failure`.
    #[inline]
    pub fn fail(&mut self, failure: Failure) {
        self.failure = Some(failure);
    }
}

/// A thread of a condition sequence: its states, `let`s and start time,
/// and how many attempts merged into it.
struct Thread<L, const W: usize> {
    states: States<W>,
    lets: L,
    start: u64,
}

/// A sequence on the condition side of an implication (`if seq!(..)`):
/// every match is checked, so its threads are independent, and a thread
/// that dies is dropped silently. Threads with equal states and `let`s
/// merge, keeping the earliest start.
pub struct Antecedent<L, const W: usize> {
    threads: Vec<Thread<L, W>>,
    next: Vec<Thread<L, W>>,
    out: Out<L, W>,
    matches: Vec<(L, u64)>,
}

impl<L, const W: usize> Default for Antecedent<L, W> {
    fn default() -> Self {
        Antecedent {
            threads: Vec::new(),
            next: Vec::new(),
            out: Out::default(),
            matches: Vec::new(),
        }
    }
}

impl<L: Clone + PartialEq, const W: usize> Antecedent<L, W> {
    fn push(threads: &mut Vec<Thread<L, W>>, states: States<W>, lets: L, start: u64) {
        if let Some(t) = threads
            .iter_mut()
            .find(|t| t.states == states && t.lets == lets)
        {
            t.start = t.start.min(start);
        } else {
            threads.push(Thread {
                states,
                lets,
                start,
            });
        }
    }

    /// Starts a thread at this edge, started at `start`: `enter` evaluates
    /// the sequence's first cycle. Returns the matches at this edge, each
    /// with its start; give the `Vec` back with [`Self::give_back`].
    pub fn start(&mut self, start: u64, enter: impl FnOnce(&mut Out<L, W>)) -> Vec<(L, u64)> {
        self.out.clear();
        enter(&mut self.out);
        for (states, lets) in self.out.pieces.drain(..) {
            Self::push(&mut self.threads, states, lets, start);
        }
        let mut matches = std::mem::take(&mut self.matches);
        matches.extend(self.out.matches.drain(..).map(|l| (l, start)));
        matches
    }

    /// Steps every thread by one edge: `step` evaluates a thread's states
    /// at this edge. Returns the matches, as [`Self::start`] does.
    pub fn step(&mut self, mut step: impl FnMut(&States<W>, &L, &mut Out<L, W>)) -> Vec<(L, u64)> {
        let mut matches = std::mem::take(&mut self.matches);
        self.next.clear();
        for thread in self.threads.drain(..) {
            self.out.clear();
            step(&thread.states, &thread.lets, &mut self.out);
            for (states, lets) in self.out.pieces.drain(..) {
                Self::push(&mut self.next, states, lets, thread.start);
            }
            matches.extend(self.out.matches.drain(..).map(|l| (l, thread.start)));
        }
        std::mem::swap(&mut self.threads, &mut self.next);
        matches
    }

    /// Gives back the `Vec` of matches, emptied, for reuse.
    pub fn give_back(&mut self, mut matches: Vec<(L, u64)>) {
        matches.clear();
        self.matches = matches;
    }

    /// The threads running.
    pub fn len(&self) -> usize {
        self.threads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }
}

/// An attempt of an expectation's sequence: its branches, one piece per
/// value of the `let`s, and its start. It succeeds when one branch
/// matches, and fails when none is left.
struct Attempt<L, const W: usize> {
    pieces: Vec<(States<W>, L)>,
    start: u64,
    merged: u32,
}

/// A sequence on the expectation side (`seq!(..)` ending a body): one
/// match suffices. Attempts with equal branches merge, keeping the
/// earliest start and counting the attempts merged.
pub struct Consequent<L, const W: usize> {
    attempts: Vec<Attempt<L, W>>,
    out: Out<L, W>,
}

impl<L, const W: usize> Default for Consequent<L, W> {
    fn default() -> Self {
        Consequent {
            attempts: Vec::new(),
            out: Out::default(),
        }
    }
}

/// An attempt of an expectation that failed: the element that failed last,
/// the attempt's start and how many attempts merged into it besides.
pub struct Failed {
    pub failure: Failure,
    pub start: u64,
    pub merged: u32,
}

impl<L: Clone + PartialEq, const W: usize> Consequent<L, W> {
    fn push(&mut self, pieces: Vec<(States<W>, L)>, start: u64, merged: u32) {
        if let Some(a) = self.attempts.iter_mut().find(|a| a.pieces == pieces) {
            a.start = a.start.min(start);
            a.merged += merged + 1;
        } else {
            self.attempts.push(Attempt {
                pieces,
                start,
                merged,
            });
        }
    }

    /// Starts an attempt at this edge, started at `start`: `enter`
    /// evaluates the sequence's first cycle. `Err` if it fails at once.
    pub fn start(&mut self, start: u64, enter: impl FnOnce(&mut Out<L, W>)) -> Result<(), Failure> {
        self.out.clear();
        enter(&mut self.out);
        if !self.out.matches.is_empty() {
            return Ok(());
        }
        if self.out.pieces.is_empty() {
            return Err(self.out.failure.expect("a branch died at an element"));
        }
        let pieces = std::mem::take(&mut self.out.pieces);
        self.push(pieces, start, 0);
        Ok(())
    }

    /// Steps every attempt by one edge: `step` evaluates a branch's states
    /// at this edge. `failed` is called for each attempt with no branch
    /// left.
    pub fn step(
        &mut self,
        mut step: impl FnMut(&States<W>, &L, &mut Out<L, W>),
        mut failed: impl FnMut(Failed),
    ) {
        let attempts = std::mem::take(&mut self.attempts);
        for attempt in attempts {
            self.out.clear();
            for (states, lets) in &attempt.pieces {
                step(states, lets, &mut self.out);
            }
            if !self.out.matches.is_empty() {
                continue;
            }
            if self.out.pieces.is_empty() {
                failed(Failed {
                    failure: self.out.failure.expect("a branch died at an element"),
                    start: attempt.start,
                    merged: attempt.merged,
                });
                continue;
            }
            let mut pieces = attempt.pieces;
            pieces.clear();
            pieces.append(&mut self.out.pieces);
            self.push(pieces, attempt.start, attempt.merged);
        }
    }

    /// The attempts running.
    pub fn len(&self) -> usize {
        self.attempts.len()
    }

    /// The attempts running: each one's start, and how many attempts merged
    /// into it besides.
    pub fn attempts(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.attempts.iter().map(|a| (a.start, a.merged))
    }

    pub fn is_empty(&self) -> bool {
        self.attempts.is_empty()
    }
}

/// The samples of a block's signals at the current edge and the `DEPTH`
/// before it, as a ring of bound signals structs: each edge reads into the
/// oldest. `past(x, n)` reads `x` from `past(n)`.
pub struct History<S> {
    ring: Vec<S>,
    head: usize,
}

impl<S: AssertionSignals> History<S> {
    /// `ring` holds `depth + 1` structs, bound and zeroed.
    #[doc(hidden)]
    pub fn new(ring: Vec<S>) -> Self {
        assert!(!ring.is_empty());
        History { ring, head: 0 }
    }

    /// The samples at the current edge.
    #[inline]
    pub fn now(&self) -> &S {
        &self.ring[self.head]
    }

    /// The samples `n` edges before the current one (`1..=depth`).
    #[inline]
    pub fn past(&self, n: usize) -> &S {
        let len = self.ring.len();
        &self.ring[(self.head + len - n % len) % len]
    }

    /// At an edge of the clock: the oldest samples become the current
    /// ones, read anew.
    #[inline]
    pub(crate) fn tick(&mut self) {
        self.head = (self.head + 1) % self.ring.len();
        self.ring[self.head].sample();
    }

    pub(crate) fn clock(&self) -> verald_host::ClockPort {
        self.now().clock()
    }

    pub(crate) fn reset(&self) -> Option<verald_host::ResetPort> {
        self.now().reset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Location;

    fn failure() -> Failure {
        Failure {
            expr: "x",
            location: Location { file: "f", line: 1 },
        }
    }

    #[test]
    fn states_advance_within_their_range() {
        let mut s = States::<2>::EMPTY;
        s.set(3);
        s.set(5);
        let next = s.advance(3, 5, false);
        assert!(next.get(4) && !next.get(5) && !next.get(6) && !next.get(3));
        let next = s.advance(3, 5, true);
        assert!(next.get(4) && next.get(5));
        // Across a word.
        let mut s = States::<2>::EMPTY;
        s.set(63);
        assert!(s.advance(60, 70, false).get(64));
        assert!(States::<2>::range(62, 65).intersects(&s));
    }

    /// `advance` one state at a time, to compare with.
    fn advance_by_bits<const W: usize>(
        s: &States<W>,
        lo: usize,
        hi: usize,
        sticky: bool,
    ) -> States<W> {
        let mut next = States::EMPTY;
        for i in lo..hi {
            if s.get(i) {
                next.set(i + 1);
            }
        }
        if sticky && s.get(hi) {
            next.set(hi);
        }
        next
    }

    #[test]
    fn a_range_spans_words() {
        let r = States::<4>::range(60, 200);
        assert_eq!(r.0, [0xf << 60, u64::MAX, u64::MAX, (1 << 9) - 1]);
        assert_eq!(States::<2>::range(64, 127).0, [0, u64::MAX]);
        assert_eq!(States::<2>::range(5, 4).0, [0, 0]);
        assert_eq!(States::<1>::range(0, 63).0, [u64::MAX]);
    }

    #[test]
    fn states_move_across_words() {
        // A range over four words, with states at each word's last bit.
        let mut s = States::<4>::EMPTY;
        for i in [10, 63, 127, 191, 200] {
            s.set(i);
        }
        // Outside the range: left as it is out of the result.
        s.set(5);
        s.set(230);
        let next = s.advance(10, 200, false);
        let set: Vec<usize> = (0..256).filter(|&i| next.get(i)).collect();
        assert_eq!(set, vec![11, 64, 128, 192]);
        let next = s.advance(10, 200, true);
        let set: Vec<usize> = (0..256).filter(|&i| next.get(i)).collect();
        assert_eq!(set, vec![11, 64, 128, 192, 200]);
    }

    #[test]
    fn advance_by_words_is_advance_by_bits() {
        // Random states and ranges, against the state-by-state version.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let s = States::<3>([next(), next(), next()]);
            let a = (next() % 192) as usize;
            let b = (next() % 192) as usize;
            let (lo, hi) = (a.min(b), a.max(b));
            let sticky = next() & 1 == 1;
            assert_eq!(
                s.advance(lo, hi, sticky),
                advance_by_bits(&s, lo, hi, sticky),
                "{lo}..={hi} sticky {sticky}"
            );
        }
    }

    #[test]
    fn attempts_in_the_same_state_merge() {
        let mut c = Consequent::<(), 1>::default();
        // Three attempts, each waiting in state 0.
        for start in [1, 2, 3] {
            c.start(start, |out| out.set(&(), 0)).unwrap();
        }
        assert_eq!(c.len(), 1);
        let mut failed = Vec::new();
        c.step(
            |_, _, out| out.fail(failure()),
            |f| failed.push((f.start, f.merged)),
        );
        assert_eq!(failed, vec![(1, 2)]);
        assert!(c.is_empty());
    }

    #[test]
    fn an_attempt_lives_while_a_branch_does() {
        let mut c = Consequent::<u8, 1>::default();
        c.start(1, |out| {
            out.set(&1, 0);
            out.set(&2, 0);
        })
        .unwrap();
        let mut failed = 0;
        // The branch with 1 dies, the one with 2 goes on.
        c.step(
            |s, l, out| {
                if *l == 1 {
                    out.fail(failure());
                } else {
                    out.or(l, s);
                }
            },
            |_| failed += 1,
        );
        assert_eq!((failed, c.len()), (0, 1));
        // It matches.
        c.step(|_, l, out| out.matched(l), |_| failed += 1);
        assert_eq!((failed, c.len()), (0, 0));
    }

    #[test]
    fn threads_with_equal_states_merge_keeping_the_earliest_start() {
        let mut a = Antecedent::<(), 1>::default();
        for start in [1, 2] {
            let m = a.start(start, |out| out.set(&(), 0));
            a.give_back(m);
        }
        assert_eq!(a.len(), 1);
        let m = a.step(|_, l, out| out.matched(l));
        assert_eq!(m, vec![((), 1)]);
    }
}
