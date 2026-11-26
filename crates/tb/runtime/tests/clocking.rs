//! Clocking blocks, driven through the exported component with
//! `veryl_component::testing::MockSim` standing in for Veryl's simulator.

use std::cell::RefCell;
use std::convert::Infallible;

use verald_tb_runtime::prelude::*;
use verald_tb_runtime::{Start, run_with_reset};
use veryl_component::Value;
use veryl_component::testing::{MockSim, RefDir};

#[modport]
struct Bus {
    #[output]
    start: bool,
    #[output]
    a: u8,
    #[input]
    done: bool,
    #[input]
    result: u16,
}

#[clocking]
struct Cb {
    #[clock]
    clk: Clock,
    #[modport]
    bus: Bus,
}

/// A clocking block over a modport and signals outside any interface.
#[clocking]
struct IrqCb {
    #[clock]
    clk: Clock,
    #[modport]
    bus: Bus,
    #[input]
    irq: bool,
    #[output]
    ack: bool,
    /// An interface's member bound on its own.
    #[input]
    done: bool,
}

/// A modport for monitoring: inputs only.
#[modport]
struct BusMonitor {
    #[input]
    done: bool,
    #[input]
    result: u16,
}

/// `#[modport]` marks a struct with no output as `InputOnly`.
#[test]
fn a_modport_of_inputs_only_is_input_only() {
    fn input_only<T: verald_tb_runtime::host::InputOnly>() {}
    input_only::<BusMonitor>();
}

/// Outputs with initial values, in a modport and on their own.
#[modport]
struct InitBus {
    #[output(init = 3)]
    a: u8,
    #[output]
    start: bool,
}

#[clocking]
struct InitCb {
    #[clock]
    clk: Clock,
    #[modport]
    bus: InitBus,
    #[output(init = true)]
    ack: bool,
}

/// A wide signal read whole.
#[clocking]
struct WideCb {
    #[clock]
    clk: Clock,
    #[input]
    wide: verald_tb_runtime::BigUint,
}

#[modport]
struct NarrowBus {
    #[input]
    result: u8,
}

#[clocking]
struct NarrowCb {
    #[clock]
    clk: Clock,
    #[modport]
    bus: NarrowBus,
}

/// A clock and a reset named otherwise than registered, mapped by paths.
#[clocking]
struct ResetCb {
    #[clock]
    main_clk: Clock,
    #[reset]
    main_rst: Reset,
    #[modport]
    bus: BusMonitor,
}

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(entry.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

struct Tests;

impl Testbench for Tests {
    fn start(start: &mut Start) -> Result<(), TestError> {
        let clk = start.clock("clk")?;
        match start.test_name() {
            "drive_and_sample" => {
                let mut cb = start.clocking::<Cb>()?;
                start.main("test", async move {
                    cb.bus.start = true;
                    cb.bus.a = 5;
                    tick!(cb);
                    log(format!("result {}", cb.bus.result));
                    cb.bus.start = false;
                    tick!(cb);
                    log(format!("result {}", cb.bus.result));
                    Ok(())
                });
            }
            "posedge" => {
                let mut cb = start.clocking::<Cb>()?;
                start.main("test", async move {
                    posedge!(cb.bus.done);
                    log(format!(
                        "done at {} with result {}",
                        cb.clock().edges(),
                        cb.bus.result
                    ));
                    Ok(())
                });
            }
            "within" => {
                let mut cb = start.clocking::<Cb>()?;
                start.main("test", async move {
                    posedge_within!(cb.bus.done, 2)?;
                    Ok(())
                });
            }
            "tick_until" => {
                let mut cb = start.clocking::<Cb>()?;
                start.main("test", async move {
                    tick_until!(cb.bus.result >= 3);
                    log(format!("at {}: {}", cb.clock().edges(), cb.bus.result));
                    Ok(())
                });
            }
            "irq" => {
                let mut cb = start.clocking_with(IrqCbPaths {
                    irq: Some("interrupt"),
                    done: Some("bus.done"),
                    ..Default::default()
                })?;
                start.main("test", async move {
                    posedge!(cb.irq);
                    log(format!(
                        "irq at {} with result {} and done {}",
                        cb.clock().edges(),
                        cb.bus.result,
                        cb.done
                    ));
                    cb.ack = true;
                    tick!(cb);
                    Ok(())
                });
            }
            "wide" => {
                let mut cb = WideCb::new(start, WideCbPaths::default())?;
                start.main("test", async move {
                    tick!(cb);
                    log(format!("{:x}", cb.wide));
                    Ok(())
                });
            }
            "init" => {
                let mut cb = start.clocking::<InitCb>()?;
                start.main("test", async move {
                    log(format!("before init: a {} ack {}", cb.bus.a, cb.ack));
                    tick!(cb);
                    cb.bus.a = 9;
                    cb.ack = false;
                    tick!(cb);
                    cb.init();
                    log(format!("after init: a {} ack {}", cb.bus.a, cb.ack));
                    tick!(cb);
                    Ok(())
                });
            }
            "narrow" => {
                let _cb = NarrowCb::new(start, NarrowCbPaths::default())?;
                start.main("test", async { Ok(()) });
            }
            "run_with_reset" => {
                let rst = start.reset("rst")?;
                let cb = start.clocking::<Cb>()?;
                start.spawn("driver", async move {
                    let mut cb = cb;
                    let never: Result<Infallible, TestError> = run_with_reset(
                        rst,
                        &mut cb,
                        |cb| {
                            log("on_reset");
                            cb.bus.start = false;
                            cb.drive();
                        },
                        async |cb| {
                            loop {
                                cb.bus.start = !cb.bus.start;
                                tick!(cb);
                                log(format!("drove {}", cb.bus.start));
                            }
                        },
                    )
                    .await;
                    never.map(|_| ())
                });
                start.main("test", async move {
                    clk.wait(4).await;
                    Ok(())
                });
            }
            "clock_paths" => {
                let cb = start.clocking_with(ResetCbPaths {
                    main_clk: Some("clk"),
                    main_rst: Some("rst"),
                    ..Default::default()
                })?;
                log(format!(
                    "clock {} reset {}",
                    cb.clock().name(),
                    cb.main_rst.name()
                ));
                start.main("test", async { Ok(()) });
            }
            "unmapped_clock" => {
                start.clocking::<ResetCb>()?;
                start.main("test", async { Ok(()) });
            }
            other => return Err(TestError::new(format!("no test `{other}`"))),
        }
        Ok(())
    }
}

verald_tb_runtime::component!(Tb, Tests);

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

/// A simulator with a clock `clk`, a reset `rst` and the interface `bus`.
fn setup() -> (MockSim, Tb) {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[str("clk"), clk]).unwrap();
    let rst = sim.reset_ref("rst");
    sim.call(&mut tb, "set_reset", &[str("rst"), rst]).unwrap();
    let bus = sim.modport_ref(
        "bus",
        "adder_if",
        "tb",
        &[
            ("start", RefDir::Output, &[1]),
            ("a", RefDir::Output, &[8]),
            ("done", RefDir::Input, &[1]),
            ("result", RefDir::Input, &[9]),
        ],
    );
    sim.call(&mut tb, "set_modport", &[str("bus"), bus])
        .unwrap();
    let irq = sim.signal_ref("interrupt", false, &[1]);
    sim.call(&mut tb, "set_input", &[str("interrupt"), irq])
        .unwrap();
    let wide = sim.signal_ref("wide", false, &[200]);
    sim.call(&mut tb, "set_input", &[str("wide"), wide])
        .unwrap();
    let ack = sim.signal_ref("ack", true, &[1]);
    sim.call(&mut tb, "set_output", &[str("ack"), ack]).unwrap();
    (sim, tb)
}

fn clock(sim: &mut MockSim, tb: &mut Tb) {
    sim.set_fired_clock("clk");
    sim.clock(tb).unwrap();
}

#[test]
fn outputs_are_driven_at_a_tick_and_inputs_sampled_after_it() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("drive_and_sample")])
        .unwrap();
    // The first tick drives before waiting.
    assert_eq!(sim.get("bus.start"), Value::from_u64(1, 1));
    assert_eq!(sim.get("bus.a"), Value::from_u64(5, 8));
    sim.set("bus.result", Value::from_u64(9, 9));
    clock(&mut sim, &mut tb);
    sim.set("bus.result", Value::from_u64(10, 9));
    clock(&mut sim, &mut tb);
    assert_eq!(sim.get("bus.start"), Value::from_u64(0, 1));
    assert_eq!(take_log(), ["result 9", "result 10"]);
    assert!(sim.finish_requested());
}

#[test]
fn posedge_waits_for_a_rise_and_samples_every_input_then() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("posedge")]).unwrap();
    for (done, result) in [(0, 1), (0, 2), (1, 3)] {
        sim.set("bus.done", Value::from_u64(done, 1));
        sim.set("bus.result", Value::from_u64(result, 9));
        clock(&mut sim, &mut tb);
    }
    assert_eq!(take_log(), ["done at 3 with result 3"]);
}

#[test]
fn a_limited_wait_times_out_as_a_fatal_error() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("within")]).unwrap();
    clock(&mut sim, &mut tb);
    clock(&mut sim, &mut tb);
    assert_eq!(
        sim.failures(),
        ["fatal: test: `cb.bus.done` not seen within 2 cycles"]
    );
}

#[test]
fn tick_until_ticks_until_the_condition_holds() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("tick_until")]).unwrap();
    for result in [1, 2, 3, 4] {
        sim.set("bus.result", Value::from_u64(result, 9));
        clock(&mut sim, &mut tb);
    }
    assert_eq!(take_log(), ["at 3: 3"]);
}

#[test]
fn a_member_too_wide_for_its_field_fails_the_start() {
    let (mut sim, mut tb) = setup();
    let error = sim.call(&mut tb, "start", &[str("narrow")]).unwrap_err();
    assert!(
        error.to_string().contains("`bus.result` is 9 bits wide"),
        "{error}"
    );
}

#[test]
fn run_with_reset_restarts_the_work_at_each_reset() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("run_with_reset")])
        .unwrap();
    clock(&mut sim, &mut tb);
    sim.set_fired_clock("rst");
    sim.reset(&mut tb).unwrap();
    // The reset drove the bus idle at once.
    assert_eq!(sim.get("bus.start"), Value::from_u64(0, 1));
    clock(&mut sim, &mut tb);
    clock(&mut sim, &mut tb);
    assert_eq!(
        take_log(),
        ["on_reset", "drove true", "on_reset", "drove true"]
    );
}

#[test]
fn signals_outside_interfaces_and_members_bind_by_path() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("irq")]).unwrap();
    for (irq, result) in [(0, 1), (1, 2)] {
        sim.set("interrupt", Value::from_u64(irq, 1));
        sim.set("bus.done", Value::from_u64(1, 1));
        sim.set("bus.result", Value::from_u64(result, 9));
        clock(&mut sim, &mut tb);
    }
    assert_eq!(take_log(), ["irq at 2 with result 2 and done true"]);
    // The acknowledge is driven by the tick that follows.
    assert_eq!(sim.get("ack"), Value::from_u64(1, 1));
}

#[test]
fn a_signal_wider_than_128_bits_reads_into_a_big_uint() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("wide")]).unwrap();
    let words = smallvec_words(&[0x1111_2222_3333_4444, 0x5555_6666_7777_8888, 0xab, 0]);
    sim.set("wide", words);
    clock(&mut sim, &mut tb);
    assert_eq!(take_log(), ["ab55556666777788881111222233334444"]);
}

fn smallvec_words(words: &[u64]) -> Value {
    Value::from_bits(words.iter().copied().collect(), Default::default(), 200)
}

#[test]
fn init_drives_the_declared_initial_values_at_once() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("init")]).unwrap();
    // The initial values are the fields' values, but not driven yet.
    assert_eq!(sim.get("bus.a"), Value::from_u64(0, 8));
    assert_eq!(sim.get("ack"), Value::from_u64(0, 1));
    clock(&mut sim, &mut tb);
    assert_eq!(sim.get("bus.a"), Value::from_u64(9, 8));
    assert_eq!(sim.get("ack"), Value::from_u64(0, 1));
    clock(&mut sim, &mut tb);
    // `init` drove them at once, within the hook.
    assert_eq!(sim.get("bus.a"), Value::from_u64(3, 8));
    assert_eq!(sim.get("ack"), Value::from_u64(1, 1));
    assert_eq!(
        take_log(),
        ["before init: a 3 ack true", "after init: a 3 ack true"]
    );
}

#[test]
fn a_clock_and_a_reset_bind_by_name_or_paths() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("clock_paths")]).unwrap();
    assert_eq!(take_log(), ["clock clk reset rst"]);
    let (mut sim, mut tb) = setup();
    let error = sim
        .call(&mut tb, "start", &[str("unmapped_clock")])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no clock is registered as `main_clk`"),
        "{error}"
    );
}
