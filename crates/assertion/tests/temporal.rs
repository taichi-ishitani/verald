//! Temporal properties, checked against tables of the signals' values at
//! each edge: which edges an attempt fails at, and which it does not.

mod common;

use common::summary;
use verald_assertion::{Assertions, assertions, blocks, onehot0};
use veryl_component::testing::MockSim;

/// `req |-> ##[1:3] ack`
#[assertions]
#[derive(Default)]
struct ReqAck {
    #[clock]
    clk: Clock,
    #[reset]
    rst: Reset,
    #[input]
    req: bool,
    #[input]
    ack: bool,
}

#[assertions]
impl ReqAck {
    #[assert]
    fn req_ack(&self) {
        if req {
            seq!(delay(1..=3) -> ack);
        }
    }
}

/// `a ##[1:2] b |-> c`: every match of the condition is checked.
#[assertions]
#[derive(Default)]
struct ThenC {
    #[clock]
    clk: Clock,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl ThenC {
    #[assert]
    fn b_then_c(&self) {
        if seq!(a -> delay(1..=2) -> b) {
            c;
        }
    }
}

/// `(req, d = data) |-> ##2 out == d`, and
/// `(a, d = data) ##1 b |-> out == d`.
#[assertions]
#[derive(Default)]
struct Keep {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    data: u8,
    #[input]
    out: u8,
}

#[assertions]
impl Keep {
    #[assert]
    fn echo(&self) {
        if req {
            let d: u8 = data;
            seq!(delay(2) -> out == d);
        }
    }

    #[assert]
    fn keep_mid_sequence(&self) {
        if seq!(a -> let d: u8 = data -> delay(1) -> b) {
            out == d;
        }
    }
}

/// `$past`, `$rose`, `$stable` and `$onehot0`.
#[assertions]
#[derive(Default)]
struct Past {
    #[clock]
    clk: Clock,
    #[reset]
    rst: Reset,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    data: u8,
    #[input]
    out: u8,
}

#[assertions]
impl Past {
    /// `out` is `data` of two edges before.
    #[assert]
    fn delayed(&self) {
        out == past(data, 2);
    }

    /// A rise of `a` comes with `b`.
    #[assert]
    fn rise(&self) {
        if rose(a) {
            b;
        }
    }

    /// While `b`, `data` holds, and at most one bit of `out` is set.
    #[assert]
    fn hold(&self) {
        if b {
            stable(data) && onehot0(out);
        }
    }
}

/// `req |=> ##[0:$] ack`: weak, never fails; and `a |-> b ##0 c`
/// fused at one edge.
#[assertions]
#[derive(Default)]
struct Unbounded {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    ack: bool,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    c: bool,
}

#[assertions]
impl Unbounded {
    #[assert]
    fn eventually_ack(&self) {
        if req {
            seq!(delay(1..) -> ack);
        }
    }

    #[assert]
    fn same_edge(&self) {
        if a {
            seq!(b -> c);
        }
    }
}

/// `req |-> ##[60:130] ack`: a range over three words of states.
#[assertions]
#[derive(Default)]
struct Long {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    ack: bool,
}

#[assertions]
impl Long {
    #[assert]
    fn late_ack(&self) {
        if req {
            seq!(delay(60..=130) -> ack);
        }
    }
}

/// `past` in a hook's `check!`.
#[assertions]
#[derive(Default)]
struct PastCheck {
    #[clock]
    clk: Clock,
    #[input]
    a: bool,
    #[input]
    b: bool,
}

#[assertions]
impl PastCheck {
    #[on_clock]
    fn b_follows_a(&mut self) {
        check!(!b || past(a), "`b` without `a` the edge before");
    }
}

verald_host::component! {
    struct Comp { assertions: Assertions<blocks![ReqAck, ThenC, Keep, Past, Unbounded, PastCheck, Long]> }

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
    ("a", 1),
    ("b", 1),
    ("c", 1),
    ("data", 8),
    ("out", 8),
];

fn setup(block: &str) -> (MockSim, Comp) {
    common::setup(SIGNALS, block)
}

fn set(sim: &mut MockSim, name: &str, value: u64) {
    common::set(sim, SIGNALS, name, value)
}

fn run(block: &str, columns: &[&str], rows: &[&[u64]]) -> Vec<String> {
    common::run::<Comp>(SIGNALS, block, columns, rows)
}

#[test]
fn a_delayed_expectation_holds_within_its_window() {
    // An `ack` 1, 2 or 3 edges after `req`.
    for gap in 1..=3 {
        let mut rows: Vec<[u64; 2]> = vec![[1, 0]];
        rows.extend((1..gap).map(|_| [0, 0]));
        rows.push([0, 1]);
        rows.push([0, 0]);
        let rows: Vec<&[u64]> = rows.iter().map(|r| &r[..]).collect();
        let failures = run("ReqAck", &["req", "ack"], &rows);
        assert!(failures.is_empty(), "gap {gap}: {failures:?}");
    }
}

#[test]
fn a_delayed_expectation_fails_at_the_end_of_its_window() {
    let failures = run(
        "ReqAck",
        &["req", "ack"],
        &[&[1, 0], &[0, 0], &[0, 0], &[0, 0], &[0, 0], &[0, 1]],
    );
    assert_eq!(
        summary(&failures),
        vec![(4, Some(1), "ack".to_string())],
        "{failures:?}"
    );
    assert!(
        failures[0].contains("assertion `ReqAck.req_ack` failed at 4 (started at 1): `ack`"),
        "{failures:?}"
    );
}

#[test]
fn each_request_is_an_attempt_of_its_own() {
    // `req` at 1 and 2, no `ack`: the attempts end at 4 and 5.
    let failures = run(
        "ReqAck",
        &["req", "ack"],
        &[&[1, 0], &[1, 0], &[0, 0], &[0, 0], &[0, 0], &[0, 0]],
    );
    assert_eq!(
        summary(&failures),
        vec![
            (4, Some(1), "ack".to_string()),
            (5, Some(2), "ack".to_string())
        ],
        "{failures:?}"
    );
    // One `ack` at 3 answers both.
    let failures = run(
        "ReqAck",
        &["req", "ack"],
        &[&[1, 0], &[1, 0], &[0, 1], &[0, 0], &[0, 0], &[0, 0]],
    );
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn a_reset_drops_the_attempts() {
    let (mut sim, mut comp) = setup("ReqAck");
    set(&mut sim, "req", 1);
    sim.set_fired_clock("clk");
    sim.clock(&mut comp).unwrap();
    set(&mut sim, "req", 0);
    sim.set_fired_clock("rst");
    sim.reset(&mut comp).unwrap();
    sim.set_fired_clock("clk");
    for _ in 0..5 {
        sim.clock(&mut comp).unwrap();
    }
    assert!(sim.failures().is_empty(), "{:?}", sim.failures());
}

/// `past` counts the edges in reset too, as SVA's `$past` samples at every
/// edge whatever `disable iff` says: the simulator calls `on_reset` instead
/// of `on_clock` at each edge while the reset is asserted.
#[test]
fn past_counts_the_edges_in_reset() {
    for (out, fails) in [(2, false), (1, true)] {
        let (mut sim, mut comp) = setup("Past");
        // Edge 1, then two edges in reset, then edge 4 out of reset.
        set(&mut sim, "data", 1);
        sim.time = 1;
        sim.set_fired_clock("clk");
        sim.clock(&mut comp).unwrap();
        sim.set_fired_clock("rst");
        for (t, data) in [(2, 2), (3, 3)] {
            set(&mut sim, "data", data);
            sim.time = t;
            sim.reset(&mut comp).unwrap();
        }
        // `data` two edges before is that of edge 2, in reset.
        set(&mut sim, "data", 4);
        set(&mut sim, "out", out);
        sim.time = 4;
        sim.set_fired_clock("clk");
        sim.clock(&mut comp).unwrap();
        assert_eq!(!sim.failures().is_empty(), fails, "{:?}", sim.failures());
    }
}

#[test]
fn every_match_of_a_condition_is_checked() {
    // `a` at 1, `b` at 2 and 3: two matches; `c` only at 2.
    let failures = run(
        "ThenC",
        &["a", "b", "c"],
        &[&[1, 0, 0], &[0, 1, 1], &[0, 1, 0], &[0, 0, 0]],
    );
    assert_eq!(
        summary(&failures),
        vec![(3, Some(1), "c".to_string())],
        "{failures:?}"
    );
    // `b` too late: no match, nothing checked.
    let failures = run(
        "ThenC",
        &["a", "b", "c"],
        &[&[1, 0, 0], &[0, 0, 0], &[0, 0, 0], &[0, 1, 0]],
    );
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn a_let_keeps_the_value_of_its_edge() {
    // `data` at a `req` comes out two edges later.
    let failures = run(
        "Keep",
        &["req", "data", "out"],
        &[&[1, 5, 0], &[1, 6, 0], &[0, 7, 5], &[0, 0, 6]],
    );
    assert!(failures.is_empty(), "{failures:?}");
    let failures = run(
        "Keep",
        &["req", "data", "out"],
        &[&[1, 5, 0], &[1, 6, 0], &[0, 7, 5], &[0, 0, 5]],
    );
    assert_eq!(
        summary(&failures),
        vec![(4, Some(2), "out == d".to_string())],
        "{failures:?}"
    );
}

#[test]
fn a_let_within_a_condition_is_kept_to_its_body() {
    let failures = run(
        "Keep",
        &["a", "b", "data", "out"],
        &[&[1, 0, 9, 0], &[0, 1, 0, 9], &[1, 0, 3, 0], &[0, 1, 0, 4]],
    );
    assert_eq!(
        summary(&failures),
        vec![(4, Some(3), "out == d".to_string())],
        "{failures:?}"
    );
}

#[test]
fn past_reads_earlier_edges() {
    // `out` follows `data` by two edges; zeros before the first.
    let failures = run(
        "Past",
        &["data", "out"],
        &[&[1, 0], &[2, 0], &[3, 1], &[4, 2], &[5, 4]],
    );
    assert_eq!(
        summary(&failures),
        vec![(5, None, "out == past(data, 2)".to_string())],
        "{failures:?}"
    );
}

#[test]
fn rose_and_stable_compare_with_the_edge_before() {
    // `a` rises at 2 without `b`; `a` high at 3 is no rise.
    let failures = run("Past", &["a", "b"], &[&[0, 0], &[1, 0], &[1, 0]]);
    assert_eq!(
        summary(&failures),
        vec![(2, None, "b".to_string())],
        "{failures:?}"
    );
    // `data` changes while `b`.
    let failures = run(
        "Past",
        &["b", "data", "out"],
        &[&[0, 1, 0], &[1, 1, 0], &[1, 2, 0], &[1, 2, 3]],
    );
    // `out` is not `data` of two edges before either: only `hold` is
    // looked at.
    let hold: Vec<String> = failures
        .into_iter()
        .filter(|f| f.contains("`Past.hold`"))
        .collect();
    let expr = "stable(data) && onehot0(out)".to_string();
    assert_eq!(
        summary(&hold),
        vec![(3, None, expr.clone()), (4, None, expr)],
        "{hold:?}"
    );
}

#[test]
fn an_unbounded_delay_is_weak() {
    let rows: Vec<[u64; 2]> = (0..50).map(|t| [u64::from(t % 3 == 0), 0]).collect();
    let rows: Vec<&[u64]> = rows.iter().map(|r| &r[..]).collect();
    let failures = run("Unbounded", &["req", "ack"], &rows);
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn elements_chained_without_a_delay_are_one_edge() {
    let failures = run(
        "Unbounded",
        &["a", "b", "c"],
        &[&[1, 1, 1], &[1, 1, 0], &[0, 0, 0]],
    );
    assert_eq!(
        summary(&failures),
        vec![(2, None, "c".to_string())],
        "{failures:?}"
    );
}

#[test]
fn past_works_in_a_hook_check() {
    // `b` at 2 follows `a` at 1; `b` at 4 follows nothing.
    let failures = run(
        "PastCheck",
        &["a", "b"],
        &[&[1, 0], &[0, 1], &[0, 0], &[0, 1]],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("check in `PastCheck` failed at 4: `b` without `a` the edge before"),
        "{failures:?}"
    );
}

/// `req` at edge 1, and `ack` at the edge given, if any; 140 edges.
fn long_rows(ack_at: Option<u64>) -> Vec<[u64; 2]> {
    (1..=140)
        .map(|t| [u64::from(t == 1), u64::from(Some(t) == ack_at)])
        .collect()
}

#[test]
fn a_range_over_several_words_of_states() {
    // Within the window (60 to 130 edges after 1): at its start, at its
    // end, and on both sides of each word boundary. At edge `t` the
    // attempt is in state `t - 2`: states 63 | 64 (edges 65 | 66) and
    // 127 | 128 (edges 129 | 130) are the last and first of their words.
    for ack_at in [61, 65, 66, 100, 129, 130, 131] {
        let rows = long_rows(Some(ack_at));
        let rows: Vec<&[u64]> = rows.iter().map(|r| &r[..]).collect();
        let failures = run("Long", &["req", "ack"], &rows);
        assert!(failures.is_empty(), "ack at {ack_at}: {failures:?}");
    }
    // Too early, or never: the attempt fails at the window's end.
    for ack_at in [Some(60), None] {
        let rows = long_rows(ack_at);
        let rows: Vec<&[u64]> = rows.iter().map(|r| &r[..]).collect();
        let failures = run("Long", &["req", "ack"], &rows);
        assert_eq!(
            summary(&failures),
            vec![(131, Some(1), "ack".to_string())],
            "ack at {ack_at:?}: {failures:?}"
        );
    }
}
