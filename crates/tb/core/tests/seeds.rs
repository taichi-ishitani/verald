//! Random stability: every component and every started sequence draws from
//! its own seed source, derived from the test's seed and its names.

use std::cell::RefCell;

use verald_random::{constraints, randomize, urandom};
use verald_tb_core::Methodology;
use verald_tb_core::prelude::*;
use verald_tb_runtime::{Clock, TestError};
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

#[randomize]
#[derive(Clone, Debug, Default)]
struct Tx {
    #[rand]
    addr: u16,
}

#[constraints]
impl Tx {
    fn small() {
        addr < 1000;
    }
}

/// Draws a number while it builds and one at each of two edges.
#[component(component)]
struct Drawer {
    clk: Clock,
    name: String,
}

#[component(component)]
impl Component for Drawer {
    type Context = (Clock, String);

    fn build(_scope: &Scope, (clk, name): (Clock, String)) -> Result<Self, TestError> {
        log(format!("{name} build {}", urandom()));
        Ok(Drawer { clk, name })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        for _ in 0..2 {
            self.clk.edge().await;
            let mut tx = Tx::default();
            tx.randomize()?;
            log(format!("{} run {}", self.name, tx.addr));
        }
        Ok(())
    }
}

#[component(sequencer(item = Tx))]
struct TxSequencer {}

#[sequence(sequencer = TxSequencer)]
struct Draws {
    label: &'static str,
}

impl Sequence for Draws {
    async fn body(&mut self) -> Result<(), TestError> {
        log(format!("{} {}", self.label, urandom()));
        Ok(())
    }
}

pub struct Config {
    clk: Clock,
    extra: bool,
}

/// Two drawers, and a third before them when `extra` is set; two sequences
/// of one type on one sequencer.
#[component(test)]
struct SeedTest {
    clk: Clock,
    sequencer: SequencerHandle<TxSequencer>,
    _drawers: Vec<Instance<Drawer>>,
}

#[component(test)]
impl Component for SeedTest {
    type Context = Config;

    fn build(scope: &Scope, config: Config) -> Result<Self, TestError> {
        let mut names = vec!["a", "b"];
        if config.extra {
            names.insert(0, "extra");
        }
        let drawers = names
            .into_iter()
            .map(|name| scope.create_instance::<Drawer>(name, (config.clk, name.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let sequencer = scope.create_instance::<TxSequencer>("sequencer", ())?;
        Ok(SeedTest {
            clk: config.clk,
            sequencer: sequencer.handle(),
            _drawers: drawers,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        Draws::new("first").start(&self.sequencer).await?;
        Draws::new("second").start(&self.sequencer).await?;
        self.clk.wait(3).await;
        Ok(())
    }
}

impl Test for SeedTest {
    fn setup(setup_context: &mut SetupContext) -> Result<Config, TestError> {
        Ok(Config {
            clk: verald_tb_runtime::testbench::registered_clock("clk")?,
            extra: setup_context.get_or::<u64>("extra", 0)? != 0,
        })
    }
}

register_test!(SeedTest);

verald_tb_runtime::component!(Tb, Methodology);

/// The log of a run with `seed`, with or without the extra drawer, sorted
/// (tasks interleave), and without the extra drawer's lines.
fn run(seed: u64, extra: bool) -> Vec<String> {
    take_log();
    let mut sim = MockSim::new();
    sim.seed = seed;
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[Value::Str("clk".into()), clk])
        .unwrap();
    sim.call(
        &mut tb,
        "set",
        &[Value::Str("extra".into()), Value::from(u64::from(extra))],
    )
    .unwrap();
    sim.call(&mut tb, "start", &[Value::Str("SeedTest".into())])
        .unwrap();
    sim.set_fired_clock("clk");
    for _ in 0..3 {
        sim.clock(&mut tb).unwrap();
    }
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    assert!(!sim.failed(), "{:?}", sim.failures());
    let mut log: Vec<String> = take_log()
        .into_iter()
        .filter(|l| !l.starts_with("extra"))
        .collect();
    log.sort();
    log
}

#[test]
fn a_seed_reproduces_a_run() {
    let first = run(7, false);
    assert_eq!(first.len(), 8, "{first:?}");
    assert_eq!(first, run(7, false));
}

#[test]
fn another_seed_draws_differently() {
    assert_ne!(run(7, false), run(8, false));
}

#[test]
fn a_component_added_elsewhere_changes_no_other_numbers() {
    assert_eq!(run(7, false), run(7, true));
}

#[test]
fn sequences_of_one_name_draw_differently() {
    let log = run(7, false);
    let first = log.iter().find(|l| l.starts_with("first")).unwrap();
    let second = log.iter().find(|l| l.starts_with("second")).unwrap();
    assert_ne!(first.split(' ').nth(1), second.split(' ').nth(1), "{log:?}");
}
