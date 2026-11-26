//! `until`, `eventually`, `never`, and what is reported at the end of the
//! test: attempts still running, incomplete or (strong) failed.

mod common;

use common::summary;
use verald_assertion::{Assertions, assertions, blocks};

/// `req |-> busy until done`, and `c |-> (busy until done) ##0 ok`.
#[assertions]
#[derive(Default)]
struct Until {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    c: bool,
    #[input]
    busy: bool,
    #[input]
    done: bool,
    #[input]
    ok: bool,
}

#[assertions]
impl Until {
    #[assert]
    fn busy_until_done(&self) {
        if req {
            until(busy, done);
        }
    }

    #[assert]
    fn busy_until_done_and_ok(&self) {
        if c {
            seq!(until(busy, done) -> ok);
        }
    }
}

/// `req |-> s_eventually ack`, `c |-> s_eventually (a ##1 b)`, and
/// `!ok` at every edge.
#[assertions]
#[derive(Default)]
struct Eventually {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    ack: bool,
    #[input]
    c: bool,
    #[input]
    a: bool,
    #[input]
    b: bool,
    #[input]
    ok: bool,
}

#[assertions]
impl Eventually {
    #[assert]
    fn req_eventually_ack(&self) {
        if req {
            eventually(ack);
        }
    }

    #[assert]
    fn c_eventually_a_then_b(&self) {
        if c {
            eventually(seq!(a -> delay(1) -> b));
        }
    }

    #[assert]
    fn not_ok(&self) {
        !ok;
    }
}

/// `not (a ##1 a && !b)`: no `a` two edges running without `b`.
#[assertions]
#[derive(Default)]
struct Never {
    #[clock]
    clk: Clock,
    #[input]
    a: bool,
    #[input]
    b: bool,
}

#[assertions]
impl Never {
    #[assert]
    fn no_double_a(&self) {
        never(seq!(a -> delay(1) -> a && !b));
    }
}

/// `req |-> ##[1:5] ack`: weak, incomplete at the end.
#[assertions]
#[derive(Default)]
struct Weak {
    #[clock]
    clk: Clock,
    #[input]
    req: bool,
    #[input]
    ack: bool,
}

#[assertions]
impl Weak {
    #[assert]
    fn req_ack(&self) {
        if req {
            seq!(delay(1..=5) -> ack);
        }
    }
}

verald_host::component! {
    struct Comp { assertions: Assertions<blocks![Until, Eventually, Never, Weak]> }

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
    ("c", 1),
    ("a", 1),
    ("b", 1),
    ("busy", 1),
    ("done", 1),
    ("ok", 1),
];

fn run(block: &str, columns: &[&str], rows: &[&[u64]]) -> Vec<String> {
    common::run::<Comp>(SIGNALS, block, columns, rows)
}

fn run_to_end(block: &str, columns: &[&str], rows: &[&[u64]]) -> (Vec<String>, Vec<String>) {
    common::run_to_end::<Comp>(SIGNALS, block, columns, rows)
}

fn s(at: u64, started: Option<u64>, expr: &str) -> (u64, Option<u64>, String) {
    (at, started, expr.to_string())
}

/// The lines logged about incomplete attempts.
fn incomplete(logs: &[String]) -> Vec<&String> {
    logs.iter().filter(|l| l.contains("incomplete")).collect()
}

#[test]
fn until_holds_its_condition_before_the_other() {
    let cols = &["req", "busy", "done"];
    // `busy` until `done` at 3; `done` at once needs no `busy`.
    assert_eq!(
        run("Until", cols, &[&[1, 1, 0], &[0, 1, 0], &[0, 0, 1]]),
        Vec::<String>::new()
    );
    assert_eq!(run("Until", cols, &[&[1, 0, 1]]), Vec::<String>::new());
    // `busy` drops before `done`.
    let failures = run("Until", cols, &[&[1, 1, 0], &[0, 0, 0], &[0, 0, 1]]);
    assert_eq!(
        summary(&failures),
        vec![s(2, Some(1), "busy")],
        "{failures:?}"
    );
    // Weak: no `done` before the end is incomplete, not a failure.
    let (failures, logs) = run_to_end("Until", cols, &[&[1, 1, 0], &[0, 1, 0]]);
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(incomplete(&logs).len(), 1, "{logs:?}");
}

#[test]
fn until_is_an_element_too() {
    let cols = &["c", "busy", "done", "ok"];
    assert_eq!(
        run("Until", cols, &[&[1, 1, 0, 0], &[0, 0, 1, 1]]),
        Vec::<String>::new()
    );
    let failures = run("Until", cols, &[&[1, 1, 0, 0], &[0, 0, 1, 0]]);
    assert_eq!(
        summary(&failures),
        vec![s(2, Some(1), "ok")],
        "{failures:?}"
    );
}

#[test]
fn eventually_fails_only_at_the_end() {
    let cols = &["req", "ack"];
    let (failures, logs) = run_to_end("Eventually", cols, &[&[1, 0], &[0, 0], &[0, 1]]);
    assert!(failures.is_empty(), "{failures:?}");
    assert!(incomplete(&logs).is_empty(), "{logs:?}");
    // No `ack`: nothing fails on the way, the attempt fails at the end
    // (time 4).
    assert!(run("Eventually", cols, &[&[1, 0], &[0, 0], &[0, 0]]).is_empty());
    let (failures, _) = run_to_end("Eventually", cols, &[&[1, 0], &[0, 0], &[0, 0]]);
    assert_eq!(
        summary(&failures),
        vec![s(4, Some(1), "eventually(ack)")],
        "{failures:?}"
    );
}

#[test]
fn eventually_takes_a_sequence() {
    let cols = &["c", "a", "b"];
    let (failures, _) = run_to_end("Eventually", cols, &[&[1, 0, 0], &[0, 1, 0], &[0, 0, 1]]);
    assert!(failures.is_empty(), "{failures:?}");
    let (failures, _) = run_to_end("Eventually", cols, &[&[1, 0, 0], &[0, 1, 0], &[0, 0, 0]]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("eventually(seq!"), "{failures:?}");
}

#[test]
fn after_a_failure_the_end_reports_what_is_left_as_incomplete() {
    // `ok` fails the test at 2; the `eventually` started at 1 is then cut
    // short, not failed.
    let (failures, logs) = run_to_end("Eventually", &["req", "ok"], &[&[1, 0], &[0, 1]]);
    assert_eq!(summary(&failures), vec![s(2, None, "! ok")], "{failures:?}");
    let incomplete = incomplete(&logs);
    assert_eq!(incomplete.len(), 1, "{logs:?}");
    assert!(
        incomplete[0].contains(
            "assertion `Eventually.req_eventually_ack`: 1 attempt incomplete at the end, \
             the earliest started at 1"
        ),
        "{logs:?}"
    );
}

#[test]
fn never_fails_where_its_sequence_matches() {
    let cols = &["a", "b"];
    assert!(run("Never", cols, &[&[1, 0], &[0, 0], &[1, 0], &[1, 1]]).is_empty());
    let failures = run("Never", cols, &[&[1, 0], &[1, 0], &[0, 0]]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    let summary = summary(&failures);
    assert_eq!((summary[0].0, summary[0].1), (2, Some(1)));
    assert!(summary[0].2.starts_with("never(seq!"), "{failures:?}");
}

#[test]
fn a_weak_attempt_left_at_the_end_is_incomplete() {
    let (failures, logs) = run_to_end("Weak", &["req", "ack"], &[&[1, 0], &[1, 0], &[0, 0]]);
    assert!(failures.is_empty(), "{failures:?}");
    let incomplete = incomplete(&logs);
    assert_eq!(incomplete.len(), 1, "{logs:?}");
    assert!(
        incomplete[0].contains("2 attempts incomplete at the end, the earliest started at 1"),
        "{logs:?}"
    );
}
