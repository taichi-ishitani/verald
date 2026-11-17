//! Default sequences from the configuration (`default_sequence = field`)
//! and from the handle, the later winning.

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

pub struct TxConfig {
    clk: Clock,
    default_sequence: DefaultSequence<TxSequencer>,
}

type TxContext = InstanceContext<TxConfig>;

#[component(sequencer(item = u32, default_sequence = default_sequence))]
pub struct TxSequencer {
    #[context]
    context: TxContext,
}

#[component(driver)]
struct TxDriver {
    #[seq_item_port]
    port: SeqItemPort<u32>,
    clk: Clock,
    name: &'static str,
}

#[component(driver)]
impl Component for TxDriver {
    type Context = (TxContext, &'static str);

    fn build(
        _scope: &Scope,
        (context, name): (TxContext, &'static str),
    ) -> Result<Self, TestError> {
        Ok(TxDriver {
            port: SeqItemPort::new(),
            clk: context.config().clk,
            name,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        loop {
            let item = self.port.get_next_item().await?;
            log(format!("{} {}", self.name, item.borrow()));
            self.clk.edge().await;
            self.port.item_done()?;
        }
    }
}

/// Sends `value` once.
#[sequence(sequencer = TxSequencer)]
struct Send {
    value: u32,
}

impl Sequence for Send {
    async fn body(&mut self) -> Result<(), TestError> {
        let mut item = self.value;
        self.start_item(&mut item).await?;
        self.finish_item(item).await?;
        Ok(())
    }
}

pub struct Setup {
    clk: Clock,
    mode: String,
}

/// Two sequencers sharing one configuration, each with its driver.
#[component(test)]
struct DefaultTest {
    clk: Clock,
}

#[component(test)]
impl Component for DefaultTest {
    type Context = Setup;

    fn build(scope: &Scope, setup: Setup) -> Result<Self, TestError> {
        let default_sequence = match setup.mode.as_str() {
            "none" => DefaultSequence::default(),
            _ => DefaultSequence::new(|| Send::new(1)),
        };
        let context = InstanceContext::new(TxConfig {
            clk: setup.clk,
            default_sequence,
        });
        for name in ["a", "b"] {
            let sequencer = scope
                .create_instance::<TxSequencer>(&format!("sequencer_{name}"), context.clone())?;
            let mut driver = scope
                .create_instance::<TxDriver>(&format!("driver_{name}"), (context.clone(), name))?;
            driver.port.connect(&sequencer.handle());
            if setup.mode == "handle" && name == "b" {
                sequencer.handle().set_default_sequence(|| Send::new(2));
            }
        }
        Ok(DefaultTest { clk: setup.clk })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.clk.wait(3).await;
        Ok(())
    }
}

impl Test for DefaultTest {
    fn setup(setup_context: &mut SetupContext) -> Result<Setup, TestError> {
        Ok(Setup {
            clk: veryl_tbf_runtime::host::registered_clock("clk")?,
            mode: setup_context.test_name().to_string(),
        })
    }
}

register_test!(config, DefaultTest, ());
register_test!(handle, DefaultTest, ());
register_test!(none, DefaultTest, ());

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
    for _ in 0..3 {
        sim.clock(&mut tb).unwrap();
    }
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    assert!(!sim.failed(), "{:?}", sim.failures());
    let mut log = take_log();
    log.sort();
    log
}

#[test]
fn every_sequencer_sharing_the_configuration_runs_its_default_sequence() {
    assert_eq!(run("config"), ["a 1", "b 1"]);
}

#[test]
fn a_handle_set_after_the_build_replaces_the_configurations() {
    assert_eq!(run("handle"), ["a 1", "b 2"]);
}

#[test]
fn none_set_runs_none() {
    assert!(run("none").is_empty());
}
