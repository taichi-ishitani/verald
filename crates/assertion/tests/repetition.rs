//! `wait`, `happen`, `repeat`, `throughout` and `ended`, checked against
//! tables of the signals' values at each edge.

mod common;

use common::summary;
use verald_assertion::{Assertions, assertions, blocks};

/// `req |-> ack[->1] ##0 ok`
#[assertions]
#[derive(Default)]
struct Waits {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    ack: bool,
    #[input]
    ok: bool,
}

#[assertions]
impl Waits {
    #[assert]
    fn ack_then_ok(&self) {
        if req {
            seq!(wait(ack) -> ok);
        }
    }
}

/// `start |-> b[->2] ##0 c`
#[assertions]
#[derive(Default)]
struct Happens {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl Happens {
    #[assert]
    fn second_b_with_c(&self) {
        if start {
            seq!(happen(2, b) -> c);
        }
    }
}

/// `start |-> b[->2:3] ##0 c`
#[assertions]
#[derive(Default)]
struct HappenRange {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl HappenRange {
    #[assert]
    fn second_or_third_b_with_c(&self) {
        if start {
            seq!(happen(2..=3, b) -> c);
        }
    }
}

/// `start |=> busy[*1:$] ##1 done`
#[assertions]
#[derive(Default)]
struct Burst {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    busy: bool,
    #[input]
    done: bool,
}

#[assertions]
impl Burst {
    #[assert]
    fn burst(&self) {
        if start {
            seq!(delay(1) -> repeat(1.., busy) -> delay(1) -> done);
        }
    }
}

/// `start |-> a[*3]`, and `start |-> (a ##1 b)[*2]`.
#[assertions]
#[derive(Default)]
struct Repeats {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl Repeats {
    #[assert]
    fn three_a(&self) {
        if start {
            seq!(repeat(3, a));
        }
    }

    #[assert]
    fn two_pairs(&self) {
        if c {
            seq!(repeat(2, seq!(a -> delay(1) -> b)));
        }
    }
}

/// `start |=> busy throughout done[->1]`, and
/// `start |-> (busy throughout ##2 1) ##0 done`.
#[assertions]
#[derive(Default)]
struct Through {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    req: bool,
    #[input]
    busy: bool,
    #[input]
    done: bool,
}

#[assertions]
impl Through {
    #[assert]
    fn busy_until_done(&self) {
        if start {
            seq!(delay(1) -> throughout(busy, wait(done)));
        }
    }

    #[assert]
    fn busy_for_two(&self) {
        if req {
            seq!(throughout(busy, delay(2)) -> done);
        }
    }
}

/// `c |-> (a ##1 b).triggered`, and every `b` after `start` with `c`.
#[assertions]
#[derive(Default)]
struct Ended {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl Ended {
    #[assert]
    fn after_a_then_b(&self) {
        if c {
            ended(seq!(a -> delay(1) -> b));
        }
    }
}

/// `start ##0 b[->1:$] |-> c`: every `b` after a `start`.
#[assertions]
#[derive(Default)]
struct EveryB {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl EveryB {
    #[assert]
    fn every_b_with_c(&self) {
        if seq!(start -> happen(1.., b)) {
            c;
        }
    }
}

/// `start |-> (d = data ##1 out == d)[*2]`: a `let` in a repetition.
#[assertions]
#[derive(Default)]
struct RepeatLet {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    data: u8,
    #[input]
    out: u8,
}

#[assertions]
impl RepeatLet {
    #[assert]
    fn echo_twice(&self) {
        if start {
            seq!(repeat(2, seq!(let d: u8 = data -> delay(1) -> out == d)));
        }
    }
}

/// Repetitions whose range starts at 0: none at all skips the element.
#[assertions]
#[derive(Default)]
struct Optional {
    #[clock]
    clk: Clock,
    #[input]
    start: bool,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl Optional {
    /// `a` 0 to 2 times, then a cycle, then `b`.
    #[assert]
    fn some_a_then_b(&self) {
        if start {
            seq!(repeat(0..=2, a) -> delay(1) -> b);
        }
    }

    /// `a` 0 or 1 time, and `b` (at the same cycle as the last `a`).
    #[assert]
    fn maybe_a_and_b(&self) {
        if c {
            seq!(happen(0..=1, a) -> b);
        }
    }
}

verald_host::component! {
    struct Comp {
        assertions: Assertions<
            blocks![Waits, Happens, HappenRange, Burst, Repeats, Through, Ended, EveryB, RepeatLet, Optional],
        >
    }

    fn start(&mut self, ctx: &mut ::veryl_component::SimCtx) -> ::veryl_component::Result<()> {
        verald_host::entry::start(ctx, None, |test| self.__verald_on_start(test))
    }

    fn bind_to_instance(
        &mut self,
        _ctx: &mut ::veryl_component::SimCtx,
        path: String,
        block: String,
    ) -> ::veryl_component::Result<()> {
        verald_host::entry::register(self.assertions.bind_to_instance(path, block))
    }
}

const SIGNALS: &[(&str, u32)] = &[
    ("req", 1),
    ("ack", 1),
    ("ok", 1),
    ("start", 1),
    ("a", 1),
    ("b", 1),
    ("c", 1),
    ("busy", 1),
    ("done", 1),
    ("data", 8),
    ("out", 8),
];

fn run(block: &str, columns: &[&str], rows: &[&[u64]]) -> Vec<String> {
    common::run::<Comp>(SIGNALS, block, columns, rows)
}

fn s(at: u64, started: Option<u64>, expr: &str) -> (u64, Option<u64>, String) {
    (at, started, expr.to_string())
}

#[test]
fn wait_matches_where_its_condition_holds() {
    let cols = &["req", "ack", "ok"];
    // At the request's own edge, and with `ack` high before it.
    assert_eq!(run("Waits", cols, &[&[1, 1, 1]]), Vec::<String>::new());
    assert_eq!(
        run("Waits", cols, &[&[0, 1, 0], &[1, 1, 1]]),
        Vec::<String>::new()
    );
    // Later.
    assert_eq!(
        run("Waits", cols, &[&[1, 0, 0], &[0, 0, 0], &[0, 1, 1]]),
        Vec::<String>::new()
    );
    // `ok` missing where `ack` comes.
    let failures = run("Waits", cols, &[&[1, 0, 0], &[0, 0, 0], &[0, 1, 0]]);
    assert_eq!(
        summary(&failures),
        vec![s(3, Some(1), "ok")],
        "{failures:?}"
    );
    // No `ack`: weak, nothing fails.
    assert_eq!(
        run("Waits", cols, &[&[1, 0, 0], &[0, 0, 0], &[0, 0, 0]]),
        Vec::<String>::new()
    );
}

#[test]
fn happen_counts_the_edges_its_condition_holds() {
    let cols = &["start", "b", "c"];
    // `b` at 2 and 4, `c` at the second.
    assert_eq!(
        run(
            "Happens",
            cols,
            &[&[1, 0, 0], &[0, 1, 0], &[0, 0, 0], &[0, 1, 1]]
        ),
        Vec::<String>::new()
    );
    // `b` held at 2 and 3 is two.
    assert_eq!(
        run("Happens", cols, &[&[1, 0, 0], &[0, 1, 0], &[0, 1, 1]]),
        Vec::<String>::new()
    );
    // `c` at the first, not the second.
    let failures = run(
        "Happens",
        cols,
        &[&[1, 0, 0], &[0, 1, 1], &[0, 0, 0], &[0, 1, 0]],
    );
    assert_eq!(summary(&failures), vec![s(4, Some(1), "c")], "{failures:?}");
}

#[test]
fn happen_over_a_range_ends_at_any_count_in_it() {
    let cols = &["start", "b", "c"];
    // `c` at the third.
    assert_eq!(
        run(
            "HappenRange",
            cols,
            &[&[1, 0, 0], &[0, 1, 0], &[0, 1, 0], &[0, 1, 1]]
        ),
        Vec::<String>::new()
    );
    // At neither: fails at the third.
    let failures = run(
        "HappenRange",
        cols,
        &[&[1, 0, 0], &[0, 1, 0], &[0, 1, 0], &[0, 1, 0]],
    );
    assert_eq!(summary(&failures), vec![s(4, Some(1), "c")], "{failures:?}");
}

#[test]
fn repeat_is_consecutive() {
    let cols = &["start", "busy", "done"];
    // Busy for two edges, done the edge after.
    assert_eq!(
        run(
            "Burst",
            cols,
            &[&[1, 0, 0], &[0, 1, 0], &[0, 1, 0], &[0, 0, 1]]
        ),
        Vec::<String>::new()
    );
    // Not busy at the first edge.
    let failures = run("Burst", cols, &[&[1, 0, 0], &[0, 0, 0]]);
    assert_eq!(
        summary(&failures),
        vec![s(2, Some(1), "busy")],
        "{failures:?}"
    );
    // Busy drops with no `done`.
    let failures = run("Burst", cols, &[&[1, 0, 0], &[0, 1, 0], &[0, 0, 0]]);
    let summary = summary(&failures);
    assert_eq!(summary.len(), 1, "{failures:?}");
    assert_eq!((summary[0].0, summary[0].1), (3, Some(1)));

    let cols = &["start", "a"];
    assert_eq!(
        run("Repeats", cols, &[&[1, 1], &[0, 1], &[0, 1]]),
        Vec::<String>::new()
    );
    let failures = run("Repeats", cols, &[&[1, 1], &[0, 0], &[0, 1]]);
    assert_eq!(
        common::summary(&failures),
        vec![s(2, Some(1), "a")],
        "{failures:?}"
    );
}

#[test]
fn repeat_of_a_sequence_starts_again_the_edge_after() {
    let cols = &["c", "a", "b"];
    // `a` 1, `b` 2, then `a` 3, `b` 4.
    assert_eq!(
        run(
            "Repeats",
            cols,
            &[&[1, 1, 0], &[0, 0, 1], &[0, 1, 0], &[0, 0, 1]]
        ),
        Vec::<String>::new()
    );
    // The second `a` at 2, with the first `b`: too early.
    let failures = run(
        "Repeats",
        cols,
        &[&[1, 1, 0], &[0, 1, 1], &[0, 0, 0], &[0, 0, 1]],
    );
    assert_eq!(summary(&failures), vec![s(3, Some(1), "a")], "{failures:?}");
}

#[test]
fn a_let_in_a_repetition_is_taken_each_time() {
    let cols = &["start", "data", "out"];
    assert_eq!(
        run(
            "RepeatLet",
            cols,
            &[&[1, 5, 0], &[0, 0, 5], &[0, 7, 0], &[0, 0, 7]]
        ),
        Vec::<String>::new()
    );
    let failures = run(
        "RepeatLet",
        cols,
        &[&[1, 5, 0], &[0, 0, 5], &[0, 7, 0], &[0, 0, 5]],
    );
    assert_eq!(
        summary(&failures),
        vec![s(4, Some(1), "out == d")],
        "{failures:?}"
    );
}

#[test]
fn throughout_holds_at_every_edge_of_its_sequence() {
    let cols = &["start", "busy", "done"];
    assert_eq!(
        run(
            "Through",
            cols,
            &[&[1, 0, 0], &[0, 1, 0], &[0, 1, 1], &[0, 0, 0]]
        ),
        Vec::<String>::new()
    );
    // `busy` drops before `done`.
    let failures = run(
        "Through",
        cols,
        &[&[1, 0, 0], &[0, 1, 0], &[0, 0, 0], &[0, 0, 1]],
    );
    assert_eq!(
        summary(&failures),
        vec![s(3, Some(1), "busy")],
        "{failures:?}"
    );
}

#[test]
fn throughout_holds_over_a_delay() {
    let cols = &["req", "busy", "done"];
    assert_eq!(
        run("Through", cols, &[&[1, 1, 0], &[0, 1, 0], &[0, 1, 1]]),
        Vec::<String>::new()
    );
    // `busy` drops within the delay.
    let failures = run("Through", cols, &[&[1, 1, 0], &[0, 0, 0], &[0, 1, 1]]);
    assert_eq!(
        summary(&failures),
        vec![s(2, Some(1), "busy")],
        "{failures:?}"
    );
}

#[test]
fn ended_is_true_where_a_match_ends() {
    let cols = &["a", "b", "c"];
    assert_eq!(
        run("Ended", cols, &[&[1, 0, 0], &[0, 1, 1]]),
        Vec::<String>::new()
    );
    let failures = run("Ended", cols, &[&[1, 0, 0], &[0, 0, 1]]);
    assert_eq!(
        summary(&failures),
        // As `stringify!` prints it.
        vec![s(2, None, "ended(seq! (a -> delay(1) -> b))")],
        "{failures:?}"
    );
}

#[test]
fn every_match_of_an_unbounded_condition_is_checked() {
    let cols = &["start", "b", "c"];
    let failures = run(
        "EveryB",
        cols,
        &[
            &[1, 0, 0],
            &[0, 1, 1],
            &[0, 0, 0],
            &[0, 1, 1],
            &[0, 1, 0],
            &[0, 0, 0],
        ],
    );
    assert_eq!(summary(&failures), vec![s(5, Some(1), "c")], "{failures:?}");
}

#[test]
fn a_repetition_from_zero_may_be_skipped() {
    let cols = &["start", "a", "b"];
    // No `a`: a cycle, then `b`.
    assert_eq!(
        run("Optional", cols, &[&[1, 0, 0], &[0, 0, 1]]),
        Vec::<String>::new()
    );
    // One or two `a`s, a cycle after the last, `b`.
    assert_eq!(
        run("Optional", cols, &[&[1, 1, 0], &[0, 0, 1]]),
        Vec::<String>::new()
    );
    assert_eq!(
        run("Optional", cols, &[&[1, 1, 0], &[0, 1, 0], &[0, 0, 1]]),
        Vec::<String>::new()
    );
    // `b` at the start's own edge does not do (SVA's `a[*0:2] ##1 b`
    // would take it): no `a`, then a cycle passes first.
    let failures = run("Optional", cols, &[&[1, 0, 1], &[0, 0, 0]]);
    assert_eq!(summary(&failures), vec![s(2, Some(1), "b")], "{failures:?}");

    let cols = &["c", "a", "b"];
    // None, or one at the same edge: `b` there.
    assert_eq!(run("Optional", cols, &[&[1, 0, 1]]), Vec::<String>::new());
    assert_eq!(
        run("Optional", cols, &[&[1, 0, 0], &[0, 1, 1]]),
        Vec::<String>::new()
    );
}
