//! Items randomized in a sequence under the sequencer's configuration, the
//! context their type declares.

use std::cell::RefCell;

use verald_random::{constraints, randomize, randomize_with};
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

pub struct BusConfig {
    clk: Clock,
    max_len: u8,
}

type BusContext = InstanceContext<BusConfig>;

#[randomize(context = BusConfig)]
#[derive(Clone, Debug, Default)]
pub struct Burst {
    #[rand]
    len: u8,
}

#[constraints]
impl Burst {
    fn limit() {
        len >= 1;
        len <= context.max_len;
    }
}

#[constraints(for Burst)]
struct Longest;

#[constraints(for Burst)]
impl Longest {
    fn longest() {
        len == context.max_len;
    }
}

#[component(sequencer(item = Burst))]
pub struct BurstSequencer {
    #[context]
    context: BusContext,
}

#[component(driver)]
struct BurstDriver {
    #[seq_item_port]
    port: SeqItemPort<Burst>,
    clk: Clock,
}

#[component(driver)]
impl Component for BurstDriver {
    type Context = BusContext;

    fn build(_scope: &Scope, context: BusContext) -> Result<Self, TestError> {
        Ok(BurstDriver {
            port: SeqItemPort::new(),
            clk: context.config().clk,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        loop {
            let item = self.port.get_next_item().await?;
            log(format!("len {}", item.borrow().len));
            self.clk.edge().await;
            self.port.item_done()?;
        }
    }
}

/// A sequence that is itself random, randomizing its items three ways.
#[randomize]
#[sequence(sequencer = BurstSequencer)]
#[derive(Default)]
struct Bursts {
    #[rand]
    count: u8,
}

#[constraints]
impl Bursts {
    fn few() {
        count.inside([3..=4]);
    }
}

impl Sequence for Bursts {
    async fn body(&mut self) -> Result<(), TestError> {
        for _ in 0..self.count {
            let mut item = Burst::default();
            self.start_item(&mut item).await?;
            self.randomize_item(&mut item)?;
            if item.len > self.config().max_len {
                return Err("the limit of the configuration is not in effect".into());
            }
            self.finish_item(item).await?;
        }
        let mut item = Burst::default();
        self.start_item(&mut item).await?;
        self.randomize_item_with(&mut item, Longest)?;
        self.finish_item(item).await?;
        let mut item = Burst::default();
        self.start_item(&mut item).await?;
        randomize_with!(item, &self.config(), {
            len == context.max_len - 1;
        })?;
        self.finish_item(item).await?;
        Ok(())
    }
}

#[component(test)]
struct ContextTest {
    sequencer: SequencerHandle<BurstSequencer>,
    clk: Clock,
}

#[component(test)]
impl Component for ContextTest {
    type Context = Clock;

    fn build(scope: &Scope, clk: Clock) -> Result<Self, TestError> {
        let context = InstanceContext::new(BusConfig { clk, max_len: 5 });
        let sequencer = scope.create_instance::<BurstSequencer>("sequencer", context.clone())?;
        let mut driver = scope.create_instance::<BurstDriver>("driver", context)?;
        driver.port.connect(&sequencer.handle());
        Ok(ContextTest {
            sequencer: sequencer.handle(),
            clk,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        let mut sequence = Bursts::default();
        sequence.randomize()?;
        log(format!("count {}", sequence.count));
        sequence.start(&self.sequencer).await?;
        self.clk.edge().await;
        Ok(())
    }
}

impl Test for ContextTest {
    fn setup(_setup_context: &mut SetupContext) -> Result<Clock, TestError> {
        verald_tb_runtime::testbench::registered_clock("clk")
    }
}

register_test!(ContextTest);

verald_tb_runtime::component!(Tb, Methodology);

#[test]
fn a_sequence_randomizes_items_under_its_sequencers_configuration() {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[Value::Str("clk".into()), clk])
        .unwrap();
    sim.call(&mut tb, "start", &[Value::Str("ContextTest".into())])
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
    let log = take_log();
    let count: usize = log[0].strip_prefix("count ").unwrap().parse().unwrap();
    assert!((3..=4).contains(&count), "{log:?}");
    assert_eq!(log.len(), 1 + count + 2, "{log:?}");
    for entry in &log[1..=count] {
        let len: u8 = entry.strip_prefix("len ").unwrap().parse().unwrap();
        assert!((1..=5).contains(&len), "{log:?}");
    }
    assert_eq!(log[count + 1], "len 5", "{log:?}");
    assert_eq!(log[count + 2], "len 4", "{log:?}");
}
