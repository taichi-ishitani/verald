//! The runtime driven through its exported component, with
//! `veryl_component::testing::MockSim` standing in for Veryl's simulator.

use std::cell::RefCell;

use verald_tb_runtime::prelude::*;
use verald_tb_runtime::{RunEnd, Start};
use veryl_component::Value;
use veryl_component::testing::MockSim;

/// The tests, chosen by the name given to `start`.
struct Tests;

thread_local! {
    /// What a test's tasks record, for the test to check.
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(entry.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

impl Testbench for Tests {
    fn option(key: &str, value: &Value) -> Result<bool, TestError> {
        match key {
            "verbose" => {
                value.as_u64().map_err(|e| TestError::new(e.to_string()))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn start(start: &mut Start) -> Result<(), TestError> {
        match start.test_name() {
            "three_edges" => {
                let clk = start.clock("clk")?;
                start.main("test", async move {
                    clk.wait(3).await;
                    log(format!("edges {}", clk.edges()));
                    Ok(())
                });
            }
            "fatal" => {
                let clk = start.clock("clk")?;
                start.spawn("env.driver", async move {
                    clk.wait(2).await;
                    Err(TestError::new("boom"))
                });
                start.main("test", async move {
                    clk.wait(10).await;
                    Ok(())
                });
            }
            "reset" => {
                let rst = start.reset("rst")?;
                start.main("test", async move {
                    rst.asserted().await;
                    log("asserted");
                    rst.released().await;
                    log("released");
                    Ok(())
                });
            }
            "forever" => {
                let clk = start.clock("clk")?;
                start.spawn("env.monitor", async move {
                    loop {
                        clk.edge().await;
                    }
                });
                start.main("test", async move {
                    std::future::pending::<()>().await;
                    Ok(())
                });
            }
            "runtime_args" => {
                let rounds: u32 = runtime_arg_value("ROUNDS")?.unwrap_or(2);
                let mode: String = runtime_arg_value("MODE")?.unwrap_or_default();
                log(format!(
                    "rounds {rounds}, mode {mode}, verbose {}, given {}",
                    runtime_arg_exists("VERBOSE"),
                    runtime_args().len()
                ));
                start.main("test", async {
                    let flag = runtime_arg("VERBOSE").is_some_and(|value| value.is_flag());
                    log(format!("flag {flag}"));
                    runtime_arg_value::<u8>("ROUNDS")?;
                    Ok(())
                });
            }
            "settings" => {
                let n = start
                    .setting("num_masters")
                    .ok_or("no num_masters")?
                    .as_u64()
                    .map_err(|e| TestError::new(e.to_string()))?;
                log(format!("num_masters {n}"));
                start.main("test", async { Ok(()) });
            }
            "fork" => {
                let clk = start.clock("clk")?;
                start.main("test", async move {
                    let ready = Fifo::new();
                    let ready = &ready;
                    let clk = &clk;
                    let collected = fork_scope(async |scope| {
                        // Delays out of order; the requests come out by delay.
                        for (req, delay) in [("a", 3), ("b", 1), ("c", 2)] {
                            scope.fork(delayed_push(clk, delay, req, ready));
                        }
                        let mut got = Vec::new();
                        for _ in 0..3 {
                            got.push(ready.get().await);
                        }
                        got
                    })
                    .await;
                    log(collected.join(" "));
                    Ok(())
                });
            }
            "join_first" => {
                let clk = start.clock("clk")?;
                let event = Event::<u32>::new();
                let trigger = event.clone();
                start.spawn("env.trigger", async move {
                    clk.wait(2).await;
                    trigger.trigger_with(7);
                    Ok(())
                });
                start.main("test", async move {
                    let (data, ()) = join(event.wait_on(), clk.wait(1)).await;
                    log(format!("data {data} at {}", clk.edges()));
                    match first(clk.wait(5), clk.wait(1)).await {
                        Either::First(()) => log("first"),
                        Either::Second(()) => log(format!("second at {}", clk.edges())),
                    }
                    Ok(())
                });
            }
            "drop_order" => {
                let clk = start.clock("clk")?;
                for name in ["older", "newer"] {
                    let guard = DropLog(name);
                    start.spawn(name, async move {
                        let _guard = guard;
                        loop {
                            clk.edge().await;
                        }
                    });
                }
                start.on_end(move |end| {
                    log(format!("end {}", matches!(end, RunEnd::Finished)));
                    Ok(())
                });
                start.main("test", async move {
                    clk.wait(1).await;
                    Ok(())
                });
            }
            "fail_outside" => {
                // A failure the runtime does not cause (an assertion's).
                let clk = start.clock("clk")?;
                start.main("test", async move {
                    clk.wait(1).await;
                    verald_tb_runtime::host::fail("an assertion failed");
                    clk.wait(100).await;
                    Ok(())
                });
            }
            "panic" => {
                let clk = start.clock("clk")?;
                start.main("test", async move {
                    clk.wait(1).await;
                    panic!("a bug in a task");
                });
            }
            other => return Err(TestError::new(format!("no test `{other}`"))),
        }
        Ok(())
    }
}

async fn delayed_push(clk: &Clock, delay: u64, req: &'static str, ready: &Fifo<&'static str>) {
    clk.wait(delay).await;
    ready.push(req);
}

struct DropLog(&'static str);

impl Drop for DropLog {
    fn drop(&mut self) {
        log(format!("drop {}", self.0));
    }
}

verald_tb_runtime::component!(Tb, Tests);

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

/// A simulator with a clock `clk` and a reset `rst` registered.
fn setup() -> (MockSim, Tb) {
    setup_with(|sim| sim)
}

/// `setup`, the simulator given runtime arguments by `args`.
fn setup_with(args: impl FnOnce(MockSim) -> MockSim) -> (MockSim, Tb) {
    take_log();
    let mut sim = args(MockSim::new());
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call_with_refs(&mut tb, "set_clock", &[str("clk").into(), clk])
        .unwrap();
    let rst = sim.reset_ref("rst");
    sim.call_with_refs(&mut tb, "set_reset", &[str("rst").into(), rst])
        .unwrap();
    (sim, tb)
}

fn clock(sim: &mut MockSim, tb: &mut Tb, n: usize) {
    sim.set_fired_clock("clk");
    for _ in 0..n {
        sim.clock(tb).unwrap();
    }
}

#[test]
fn the_run_phase_ends_when_the_main_task_returns() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("three_edges")]).unwrap();
    clock(&mut sim, &mut tb, 2);
    assert!(!sim.finish_requested());
    clock(&mut sim, &mut tb, 1);
    assert!(sim.finish_requested());
    assert!(!sim.failed(), "{:?}", sim.failures());
    assert_eq!(take_log(), ["edges 3"]);
    sim.finish(&mut tb).unwrap();
    assert!(!sim.failed(), "{:?}", sim.failures());
}

#[test]
fn an_error_from_a_task_is_fatal() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("fatal")]).unwrap();
    clock(&mut sim, &mut tb, 2);
    assert_eq!(sim.failures(), ["fatal: env.driver: boom"]);
}

#[test]
fn reset_is_asserted_by_on_reset_and_released_by_on_clock() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("reset")]).unwrap();
    sim.set_fired_clock("rst");
    sim.reset(&mut tb).unwrap();
    sim.reset(&mut tb).unwrap();
    assert_eq!(take_log(), ["asserted"]);
    clock(&mut sim, &mut tb, 1);
    assert_eq!(take_log(), ["released"]);
    assert!(sim.finish_requested());
}

#[test]
fn a_timeout_is_fatal_and_lists_the_tasks_alive() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "option", &[str("timeout"), Value::from(5u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("forever")]).unwrap();
    // The fifth cycle is still allowed; the sixth is past the timeout.
    clock(&mut sim, &mut tb, 5);
    assert!(!sim.failed());
    clock(&mut sim, &mut tb, 1);
    assert_eq!(
        sim.failures(),
        ["fatal: timeout after 5 cycles of `clk`; tasks alive: env.monitor, test"]
    );
}

#[test]
fn the_timeout_counts_the_master_clock() {
    let (mut sim, mut tb) = setup();
    let fast = sim.clock_ref("fast");
    sim.call_with_refs(&mut tb, "set_clock", &[str("fast").into(), fast])
        .unwrap();
    let error = sim
        .call(&mut tb, "set_master_clock", &[str("slow")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no clock is registered as `slow`"),
        "{error}"
    );
    sim.call(&mut tb, "set_master_clock", &[str("fast")])
        .unwrap();
    sim.call(&mut tb, "option", &[str("timeout"), Value::from(2u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("forever")]).unwrap();
    // `clk`, registered first, is not counted once `fast` is the master.
    clock(&mut sim, &mut tb, 10);
    assert!(!sim.failed());
    sim.set_fired_clock("fast");
    for _ in 0..3 {
        sim.clock(&mut tb).unwrap();
    }
    assert_eq!(
        sim.failures(),
        ["fatal: timeout after 2 cycles of `fast`; tasks alive: env.monitor, test"]
    );
}

#[test]
fn a_timeout_of_0_is_no_limit() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "option", &[str("timeout"), Value::from(0u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("forever")]).unwrap();
    clock(&mut sim, &mut tb, 100);
    assert!(!sim.failed());
}

#[test]
fn a_finish_before_the_test_ends_is_unexpected() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("forever")]).unwrap();
    clock(&mut sim, &mut tb, 3);
    sim.finish(&mut tb).unwrap();
    assert_eq!(sim.failures().len(), 1);
    assert!(sim.failures()[0].starts_with("unexpected finish"));
    assert!(sim.failures()[0].ends_with("tasks alive: env.monitor, test"));
}

#[test]
fn a_finish_after_a_failure_is_expected() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("fail_outside")]).unwrap();
    clock(&mut sim, &mut tb, 2);
    sim.finish(&mut tb).unwrap();
    assert_eq!(sim.failures(), ["an assertion failed"]);
}

#[test]
fn a_finish_without_start_fails() {
    let (mut sim, mut tb) = setup();
    sim.finish(&mut tb).unwrap();
    assert_eq!(
        sim.failures(),
        ["the testbench was not started: call `start`"]
    );
}

#[test]
fn settings_are_read_by_the_test() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "set", &[str("num_masters"), Value::from(4u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("settings")]).unwrap();
    assert_eq!(take_log(), ["num_masters 4"]);
}

#[test]
fn a_setting_no_one_reads_fails_the_start() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "set", &[str("num_master"), Value::from(4u64)])
        .unwrap();
    let error = sim
        .call(&mut tb, "start", &[str("three_edges")])
        .unwrap_err();
    assert!(error.to_string().contains("`num_master`"), "{error}");
}

#[test]
fn options_are_checked_at_once() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "option", &[str("verbose"), Value::from(1u64)])
        .unwrap();
    let error = sim
        .call(&mut tb, "option", &[str("timout"), Value::from(1u64)])
        .unwrap_err();
    assert!(
        error.to_string().contains("unknown option `timout`"),
        "{error}"
    );
    let error = sim
        .call(&mut tb, "option", &[str("timeout"), str("long")])
        .unwrap_err();
    assert!(error.to_string().contains("option `timeout`"), "{error}");
}

#[test]
fn registration_is_before_start_only() {
    let (mut sim, mut tb) = setup();
    let error = sim
        .call(&mut tb, "set_clock", &[str("clk"), Value::unit()])
        .unwrap_err();
    assert!(!error.to_string().is_empty());
    sim.call(&mut tb, "start", &[str("three_edges")]).unwrap();
    let error = sim
        .call(&mut tb, "start", &[str("three_edges")])
        .unwrap_err();
    assert!(error.to_string().contains("before `start`"), "{error}");
}

#[test]
fn forked_tasks_run_alongside_the_body() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("fork")]).unwrap();
    clock(&mut sim, &mut tb, 3);
    assert_eq!(take_log(), ["b c a"]);
    assert!(sim.finish_requested());
}

#[test]
fn join_waits_for_both_and_first_for_either() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("join_first")]).unwrap();
    clock(&mut sim, &mut tb, 3);
    assert_eq!(take_log(), ["data 7 at 2", "second at 3"]);
}

#[test]
fn tasks_are_dropped_newest_first_before_the_end_handler() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("drop_order")]).unwrap();
    clock(&mut sim, &mut tb, 1);
    assert_eq!(take_log(), ["drop newer", "drop older", "end true"]);
}

#[test]
fn a_panic_poisons_the_runtime() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("panic")]).unwrap();
    // The mock does not catch panics as Veryl does; catch it here.
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        clock(&mut sim, &mut tb, 1);
    }));
    assert!(unwound.is_err());
    // Later hooks do nothing, and the finish reports no further error.
    clock(&mut sim, &mut tb, 1);
    sim.finish(&mut tb).unwrap();
    assert!(!sim.failed(), "{:?}", sim.failures());
}

#[test]
fn a_test_reads_runtime_arguments() {
    let (mut sim, mut tb) = setup_with(|sim| {
        sim.runtime_arg("ROUNDS", "8")
            .runtime_flag("VERBOSE")
            .runtime_arg("MODE", "fast")
            .runtime_arg("ROUNDS", "0x200")
    });
    sim.call(&mut tb, "start", &[str("runtime_args")]).unwrap();
    assert_eq!(
        take_log(),
        ["rounds 512, mode fast, verbose true, given 4", "flag true"]
    );
    // 512 does not fit `u8`: the main task fails with the argument's name.
    assert_eq!(sim.failures().len(), 1);
    assert!(
        sim.failures()[0].contains("runtime argument `ROUNDS`: value `0x200` does not fit"),
        "{:?}",
        sim.failures()
    );
}

#[test]
fn verald_testname_overrides_the_test_given_to_start() {
    let (mut sim, mut tb) = setup_with(|sim| sim.runtime_arg("VERALD_TESTNAME", "three_edges"));
    sim.call(&mut tb, "start", &[str("forever")]).unwrap();
    clock(&mut sim, &mut tb, 3);
    assert_eq!(take_log(), ["edges 3"]);
    assert!(sim.finish_requested());
}

#[test]
fn verald_options_override_the_test_module() {
    let (mut sim, mut tb) = setup_with(|sim| sim.runtime_arg("VERALD_TIMEOUT", "5"));
    sim.call(&mut tb, "option", &[str("timeout"), Value::from(100u64)])
        .unwrap();
    sim.call(&mut tb, "start", &[str("forever")]).unwrap();
    clock(&mut sim, &mut tb, 6);
    assert_eq!(
        sim.failures(),
        ["fatal: timeout after 5 cycles of `clk`; tasks alive: env.monitor, test"]
    );
}

#[test]
fn verald_options_reach_the_testbench() {
    let (mut sim, mut tb) = setup_with(|sim| sim.runtime_arg("VERALD_VERBOSE", "1"));
    sim.call(&mut tb, "start", &[str("three_edges")]).unwrap();
    let (mut sim, mut tb) = setup_with(|sim| sim.runtime_arg("VERALD_VERBOSE", "on"));
    let error = sim
        .call(&mut tb, "start", &[str("three_edges")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("runtime argument `VERALD_VERBOSE`: option `verbose`"),
        "{error}"
    );
}

#[test]
fn unknown_and_valueless_verald_options_fail_the_start() {
    let (mut sim, mut tb) = setup_with(|sim| sim.runtime_arg("VERALD_TIMOUT", "5"));
    let error = sim
        .call(&mut tb, "start", &[str("three_edges")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("runtime argument `VERALD_TIMOUT`: unknown option `timout`"),
        "{error}"
    );
    let (mut sim, mut tb) = setup_with(|sim| sim.runtime_flag("VERALD_TIMEOUT"));
    let error = sim
        .call(&mut tb, "start", &[str("three_edges")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("runtime argument `VERALD_TIMEOUT`: an option takes a value"),
        "{error}"
    );
}
