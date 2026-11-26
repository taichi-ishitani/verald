//! A component made by `component!` over two subscribers, without the
//! testbench, driven by `veryl_component::testing::MockSim`.

use std::cell::RefCell;

use verald_host::{self as host, HookSubscriber, HostError, Signals};
use veryl_component::Value;
use veryl_component::testing::MockSim;

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(entry.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// Logs each hook under its name; fails `on_clock` when told to.
#[derive(Default)]
struct Logger<const N: char> {
    fail_on_clock: bool,
}

impl<const N: char> HookSubscriber for Logger<N> {
    fn on_init(&mut self) -> Result<(), HostError> {
        log(format!("{N} init"));
        Ok(())
    }
    fn on_start(&mut self, test: Option<&str>) -> Result<(), HostError> {
        // The registrations are complete and readable here.
        let clk = host::Registered.clock_port("clk").is_ok();
        log(format!("{N} start {test:?} clk {clk}"));
        if N == 'b' {
            self.fail_on_clock = host::runtime_arg("FAIL").is_some();
        }
        Ok(())
    }
    fn on_clock(&mut self) -> Result<(), HostError> {
        log(format!("{N} clock"));
        if self.fail_on_clock {
            return Err(HostError::new("b failed"));
        }
        Ok(())
    }
    fn on_finish(&mut self) -> Result<(), HostError> {
        log(format!("{N} finish failed {}", host::failed()));
        Ok(())
    }
}

verald_host::component! {
    struct Comp { a: Logger<'a'>, b: Logger<'b'> }

    fn start(&mut self, ctx: &mut ::veryl_component::SimCtx) -> ::veryl_component::Result<()> {
        verald_host::entry::start(ctx, None, |test| self.__verald_on_start(test))
    }
}

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn setup(args: impl FnOnce(MockSim) -> MockSim) -> (MockSim, Comp) {
    take_log();
    let mut sim = args(MockSim::new());
    let mut comp = sim.build::<Comp>().unwrap();
    sim.init(&mut comp).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut comp, "set_clock", &[str("clk"), clk])
        .unwrap();
    (sim, comp)
}

#[test]
fn hooks_reach_the_subscribers_in_order() {
    let (mut sim, mut comp) = setup(|sim| sim);
    sim.call(&mut comp, "start", &[]).unwrap();
    sim.set_fired_clock("clk");
    sim.clock(&mut comp).unwrap();
    sim.finish(&mut comp).unwrap();
    assert_eq!(
        take_log(),
        [
            "a init",
            "b init",
            "a start None clk true",
            "b start None clk true",
            "a clock",
            "b clock",
            "a finish failed false",
            "b finish failed false",
        ]
    );
}

#[test]
fn start_closes_the_registrations() {
    let (mut sim, mut comp) = setup(|sim| sim);
    let clk = sim.clock_ref("clk2");
    sim.call(&mut comp, "start", &[]).unwrap();
    let error = sim
        .call(&mut comp, "set_clock", &[str("clk2"), clk])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("`set_clock` is only allowed before `start`"),
        "{error}"
    );
    let error = sim.call(&mut comp, "start", &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("`start` is only allowed before `start`"),
        "{error}"
    );
}

#[test]
fn a_name_is_registered_once() {
    let (mut sim, mut comp) = setup(|sim| sim);
    let rst = sim.reset_ref("rst");
    let error = sim
        .call(&mut comp, "set_reset", &[str("clk"), rst])
        .unwrap_err();
    assert!(
        error.to_string().contains("`clk` is already registered"),
        "{error}"
    );
}

#[test]
fn a_subscribers_error_is_a_failure() {
    let (mut sim, mut comp) = setup(|sim| sim.runtime_flag("FAIL"));
    sim.call(&mut comp, "start", &[]).unwrap();
    take_log();
    sim.set_fired_clock("clk");
    let error = sim.clock(&mut comp).unwrap_err();
    assert!(error.to_string().contains("b failed"), "{error}");
    sim.finish(&mut comp).unwrap();
    assert_eq!(
        take_log(),
        [
            "a clock",
            "b clock",
            "a finish failed true",
            "b finish failed true",
        ]
    );
}
