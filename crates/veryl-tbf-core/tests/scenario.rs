//! A test's three run steps, and tests made from a base test and scenarios
//! by `#[component(test(base = .., ..))]`.

use std::cell::RefCell;

use veryl_component::Value;
use veryl_component::testing::MockSim;
use veryl_tbf_core::Methodology;
use veryl_tbf_core::prelude::*;
use veryl_tbf_runtime::{Clock, TestError};

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(entry.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

pub struct BaseConfig {
    clk: Clock,
    label: &'static str,
}

/// A child, to show the base keeps its place in the hierarchy.
#[component(component)]
struct Child {}

#[component(component, passive)]
impl Component for Child {
    type Context = ();

    fn build(_scope: &Scope, (): ()) -> Result<Self, TestError> {
        Ok(Child {})
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        log(format!("check {}", check_context.path()));
    }
}

/// A simple test: all three steps written directly.
#[component(test)]
struct BaseTest {
    #[allow(dead_code)]
    child: Instance<Child>,
    clk: Clock,
    label: &'static str,
}

#[component(test)]
impl Component for BaseTest {
    type Context = BaseConfig;

    fn setup(_setup_context: &mut SetupContext) -> Result<BaseConfig, TestError> {
        Ok(BaseConfig {
            clk: veryl_tbf_runtime::host::registered_clock("clk")?,
            label: "plain",
        })
    }

    fn build(scope: &Scope, config: BaseConfig) -> Result<Self, TestError> {
        Ok(BaseTest {
            child: scope.create_instance::<Child>("child", ())?,
            clk: config.clk,
            label: config.label,
        })
    }

    async fn pre_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.clk.edge().await;
        log("base pre_run");
        Ok(())
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.clk.edge().await;
        log(format!("base run ({})", self.label));
        Ok(())
    }

    async fn post_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.clk.edge().await;
        log("base post_run");
        Ok(())
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        log(format!("check {}", check_context.path()));
    }
}

/// A spec written for the base.
struct Labelled;

impl Spec<BaseTest> for Labelled {
    fn pre_build(&self, config: &mut BaseConfig) {
        config.label = "labelled";
    }
    fn post_build(&self, test: &mut BaseTest) {
        log(format!("post_build {}", test.label));
    }
}

#[derive(Default)]
struct Body {
    steps: u32,
}

impl Scenario for Body {
    type Test = BaseTest;

    async fn body(
        &mut self,
        test: &mut BaseTest,
        run_context: &mut RunContext,
    ) -> Result<(), TestError> {
        for _ in 0..2 {
            test.clk.edge().await;
            self.steps += 1;
        }
        log(format!(
            "scenario body ({}, {} steps, in {})",
            test.label,
            self.steps,
            run_context.path()
        ));
        Ok(())
    }
}

#[derive(Default)]
struct Prepare {}

impl Scenario for Prepare {
    type Test = BaseTest;

    async fn body(
        &mut self,
        test: &mut BaseTest,
        _run_context: &mut RunContext,
    ) -> Result<(), TestError> {
        log(format!("scenario prepare ({})", test.label));
        Ok(())
    }
}

/// The base's preparation and wind-up, a scenario for the body.
#[component(test(base = BaseTest, run = Body))]
struct BodyTest {}

/// A scenario for the preparation too, on a test made from a base.
#[component(test(base = BodyTest, pre_run = Prepare))]
struct LayeredTest {
    #[allow(dead_code)]
    extra: u32,
}

/// Its own `run` and `post_run`, as methods; the latter calls the base's.
#[component(test(base = BaseTest, run, post_run))]
struct OwnStepsTest {}

impl OwnStepsTest {
    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.clk.edge().await;
        log(format!("own run ({})", self.label));
        Ok(())
    }

    async fn post_run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> {
        log("own post_run");
        Test::post_run(&mut self.base, run_context).await
    }
}

register_test!(BaseTest);
register_test!(OwnStepsTest);
register_test!(BodyTest);
register_test!(LabelledBody, BodyTest, Labelled);
register_test!(LayeredTest);

veryl_tbf_runtime::component!(Tb, Methodology);

fn run(test: &str) -> Vec<String> {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[Value::Str("clk".into()), clk])
        .unwrap();
    sim.call(&mut tb, "start", &[Value::Str(test.into())])
        .unwrap();
    sim.set_fired_clock("clk");
    for _ in 0..10 {
        if sim.finish_requested() {
            break;
        }
        sim.clock(&mut tb).unwrap();
    }
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    assert!(!sim.failed(), "{:?}", sim.failures());
    take_log()
}

#[test]
fn a_test_runs_pre_run_run_and_post_run_in_turn() {
    assert_eq!(
        run("BaseTest"),
        [
            "base pre_run",
            "base run (plain)",
            "base post_run",
            "check child",
            "check BaseTest",
        ]
    );
}

#[test]
fn a_scenario_replaces_its_step_and_the_base_keeps_the_others() {
    assert_eq!(
        run("BodyTest"),
        [
            "base pre_run",
            "scenario body (plain, 2 steps, in BodyTest)",
            "base post_run",
            "check child",
            "check BodyTest",
        ]
    );
}

#[test]
fn a_spec_for_the_base_serves_a_test_made_from_it() {
    assert_eq!(
        run("LabelledBody"),
        [
            "post_build labelled",
            "base pre_run",
            "scenario body (labelled, 2 steps, in LabelledBody)",
            "base post_run",
            "check child",
            "check LabelledBody",
        ]
    );
}

#[test]
fn a_test_made_from_a_base_may_be_a_base() {
    assert_eq!(
        run("LayeredTest"),
        [
            "scenario prepare (plain)",
            "scenario body (plain, 2 steps, in LayeredTest)",
            "base post_run",
            "check child",
            "check LayeredTest",
        ]
    );
}

#[test]
fn a_test_made_from_a_base_may_write_its_steps_as_methods() {
    assert_eq!(
        run("OwnStepsTest"),
        [
            "base pre_run",
            "own run (plain)",
            "own post_run",
            "base post_run",
            "check child",
            "check OwnStepsTest",
        ]
    );
}
