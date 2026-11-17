//! The methodology driven through the exported component, with
//! `veryl_component::testing::MockSim` standing in for Veryl's simulator.

use std::cell::RefCell;

use veryl_component::Value;
use veryl_component::testing::MockSim;
use veryl_tbf_core::prelude::*;
use veryl_tbf_core::{Kind, Methodology};
use veryl_tbf_runtime::{Clock, Signals, TestError};

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(entry.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// Counts clock edges.
struct Counter {
    clk: Clock,
    count: u64,
}

impl Component for Counter {
    type Context = Clock;

    fn build(_scope: &Scope, clk: Clock) -> Result<Self, TestError> {
        Ok(Counter { clk, count: 0 })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        loop {
            self.clk.edge().await;
            self.count += 1;
        }
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        log(format!("check {} ({})", check_context.path(), self.count));
    }

    fn report(&self, report_context: &ReportContext) {
        log(format!("report {}", report_context.path()));
    }
}

/// Collects what the test sends it; has no run.
#[derive(Default)]
struct Board {
    seen: Vec<u64>,
    fail: bool,
}

impl Component for Board {
    type Context = ();
    const KIND: Kind = Kind::Scoreboard;
    const PASSIVE: bool = true;

    fn build(_scope: &Scope, (): ()) -> Result<Self, TestError> {
        Ok(Board::default())
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        log(format!("check {} {:?}", check_context.path(), self.seen));
        if self.fail {
            check_context.error("told to fail");
        }
    }
}

struct EnvConfig {
    clk: Clock,
    counters: usize,
    broken: bool,
}

struct Env {
    counters: Vec<Instance<Counter>>,
    board: Instance<Board>,
}

impl Component for Env {
    type Context = EnvConfig;
    const KIND: Kind = Kind::Env;

    fn build(scope: &Scope, config: EnvConfig) -> Result<Self, TestError> {
        let counters =
            scope.create_instances::<Counter>("counter", vec![config.clk; config.counters])?;
        let board = scope.create_instance::<Board>("board", ())?;
        if config.broken {
            scope.create_instance::<Broken>("broken", ())?;
        }
        Ok(Env { counters, board })
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        log(format!("check {}", check_context.path()));
    }
}

/// Fails to build.
struct Broken;

impl Component for Broken {
    type Context = ();

    fn build(scope: &Scope, (): ()) -> Result<Self, TestError> {
        scope.interface("missing")?;
        Ok(Broken)
    }
}

struct BasicConfig {
    env: EnvConfig,
    cycles: u64,
    deref_in_run: bool,
    errors: u64,
}

/// Runs for a number of cycles, then sends the board what it saw.
#[component(test)]
struct BasicTest {
    env: Instance<Env>,
    clk: Clock,
    cycles: u64,
    deref_in_run: bool,
    errors: u64,
}

impl Component for BasicTest {
    type Context = BasicConfig;
    const KIND: Kind = Kind::Test;

    fn build(scope: &Scope, config: BasicConfig) -> Result<Self, TestError> {
        let clk = config.env.clk;
        let env = scope.create_instance::<Env>("env", config.env)?;
        Ok(BasicTest {
            env,
            clk,
            cycles: config.cycles,
            deref_in_run: config.deref_in_run,
            errors: config.errors,
        })
    }

    async fn run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> {
        for _ in 0..self.errors {
            self.clk.edge().await;
            run_context.error("an error");
        }
        self.clk.wait(self.cycles).await;
        if self.deref_in_run {
            let _ = self.env.board.seen.len();
        }
        let env = self.env.borrow();
        env.board.borrow_mut().seen.push(self.cycles);
        Ok(())
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        let counts: Vec<u64> = self.env.counters.iter().map(|c| c.count).collect();
        log(format!("check {} {counts:?}", check_context.path()));
    }
}

impl Test for BasicTest {
    fn setup(setup_context: &mut SetupContext) -> Result<BasicConfig, TestError> {
        Ok(BasicConfig {
            env: EnvConfig {
                clk: Signals::clock(&ScopeLess, "clk")?,
                counters: setup_context.get_or("counters", 2)?,
                broken: false,
            },
            cycles: setup_context.get_or("cycles", 3)?,
            deref_in_run: false,
            errors: 0,
        })
    }
}

/// Looks clocks up outside a scope, for `setup`.
struct ScopeLess;

impl Signals for ScopeLess {
    fn clock(&self, name: &str) -> Result<Clock, TestError> {
        veryl_tbf_runtime::host::registered_clock(name)
    }
    fn interface(&self, name: &str) -> Result<veryl_component::ModportRef, TestError> {
        veryl_tbf_runtime::host::registered_interface(name)
    }
    fn input(&self, path: &str) -> Result<veryl_component::InputPort, TestError> {
        veryl_tbf_runtime::host::registered_input(path)
    }
    fn output(&self, path: &str) -> Result<veryl_component::OutputPort, TestError> {
        veryl_tbf_runtime::host::registered_output(path)
    }
}

/// Makes the board report an error.
struct FailingBoard;

impl Spec<BasicTest> for FailingBoard {
    fn post_build(&self, test: &mut BasicTest) {
        test.env.board.fail = true;
    }
}

/// Logs its order.
struct Note(&'static str);

impl Spec<BasicTest> for Note {
    fn pre_build(&self, config: &mut BasicConfig) {
        log(format!("pre {}", self.0));
        config.cycles += 1;
    }
    fn post_build(&self, _test: &mut BasicTest) {
        log(format!("post {}", self.0));
    }
}

struct BreakEnv;

impl Spec<BasicTest> for BreakEnv {
    fn pre_build(&self, config: &mut BasicConfig) {
        config.env.broken = true;
    }
}

struct DerefInRun;

impl Spec<BasicTest> for DerefInRun {
    fn pre_build(&self, config: &mut BasicConfig) {
        config.deref_in_run = true;
    }
}

struct ThreeErrors;

impl Spec<BasicTest> for ThreeErrors {
    fn pre_build(&self, config: &mut BasicConfig) {
        config.errors = 3;
    }
}

register_test!(BasicTest);
register_test!(FailingTest, BasicTest, FailingBoard);
register_test!(BundledTest, BasicTest, bundled_specs![Note("a"), Note("b")]);
register_test!(BrokenTest, BasicTest, BreakEnv);
register_test!(DerefTest, BasicTest, DerefInRun);
register_test!(ErrorsTest, BasicTest, ThreeErrors);

veryl_tbf_runtime::component!(Tb, Methodology);

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn setup() -> (MockSim, Tb) {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[str("clk"), clk]).unwrap();
    (sim, tb)
}

fn clock(sim: &mut MockSim, tb: &mut Tb, n: usize) {
    sim.set_fired_clock("clk");
    for _ in 0..n {
        sim.clock(tb).unwrap();
    }
}

#[test]
fn a_test_runs_until_its_run_returns_then_checks_children_first() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("BasicTest")]).unwrap();
    clock(&mut sim, &mut tb, 2);
    assert!(!sim.finish_requested());
    clock(&mut sim, &mut tb, 1);
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    assert!(!sim.failed(), "{:?}", sim.failures());
    assert_eq!(
        take_log(),
        [
            "check env.counter[0] (3)",
            "check env.counter[1] (3)",
            "check env.board [3]",
            "check env",
            "check BasicTest [3, 3]",
            "report env.counter[0]",
            "report env.counter[1]",
        ]
    );
    assert!(sim.logs().iter().any(|l| l.contains("report summary")));
}

#[test]
fn settings_reach_the_setup() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "set", &[str("cycles"), Value::from(1u64)])
        .unwrap();
    sim.call(&mut tb, "set", &[str("counters"), Value::from(1u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("BasicTest")]).unwrap();
    clock(&mut sim, &mut tb, 1);
    assert!(sim.finish_requested());
    assert!(take_log().contains(&"check BasicTest [1]".to_string()));
}

#[test]
fn an_error_in_check_fails_the_test() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("FailingTest")]).unwrap();
    clock(&mut sim, &mut tb, 3);
    assert_eq!(
        sim.failures(),
        ["1 error; the first: env.board: told to fail"]
    );
}

#[test]
fn bundled_specs_apply_in_order() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("BundledTest")]).unwrap();
    assert_eq!(take_log(), ["pre a", "pre b", "post a", "post b"]);
    // Each spec added a cycle.
    clock(&mut sim, &mut tb, 5);
    assert!(sim.finish_requested());
    let log = take_log();
    assert!(
        log.contains(&"check BundledTest [5, 5]".to_string()),
        "{log:?}"
    );
}

#[test]
fn an_unknown_test_lists_the_registered_ones() {
    let (mut sim, mut tb) = setup();
    let error = sim.call(&mut tb, "start", &[str("NoTest")]).unwrap_err();
    assert!(
        error.to_string().contains(
            "registered: BasicTest, BrokenTest, BundledTest, DerefTest, ErrorsTest, FailingTest"
        ),
        "{error}"
    );
}

#[test]
fn a_build_error_names_the_component_being_built() {
    let (mut sim, mut tb) = setup();
    let error = sim
        .call(&mut tb, "start", &[str("BrokenTest")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("building `env.broken`: no interface is registered as `missing`"),
        "{error}"
    );
}

#[test]
fn deref_during_the_run_phase_panics() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("DerefTest")]).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        clock(&mut sim, &mut tb, 3);
    }))
    .unwrap_err();
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("`env`"), "{message}");
    assert!(message.contains("use `borrow()`"), "{message}");
}

#[test]
fn the_error_limit_ends_the_run_with_a_fatal_error() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "option", &[str("max_errors"), Value::from(2u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("ErrorsTest")]).unwrap();
    clock(&mut sim, &mut tb, 3);
    assert_eq!(
        sim.failures(),
        ["fatal: ErrorsTest: the error limit is reached (2 errors); the last: an error"]
    );
}

#[test]
fn errors_are_counted_whatever_is_printed() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "option", &[str("log_level"), str("error")])
        .unwrap();
    sim.call(&mut tb, "start", &[str("ErrorsTest")]).unwrap();
    clock(&mut sim, &mut tb, 6);
    assert_eq!(
        sim.failures(),
        ["3 errors; the first: ErrorsTest: an error"]
    );
    let error = sim
        .call(&mut tb, "option", &[str("log_level"), str("loud")])
        .unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn an_unknown_option_lists_every_known_one() {
    let (mut sim, mut tb) = setup();
    let error = sim
        .call(&mut tb, "option", &[str("max_error"), Value::from(1u64)])
        .unwrap_err()
        .to_string();
    assert!(error.contains("`timeout`"), "{error}");
    assert!(error.contains("`max_errors`"), "{error}");
}
