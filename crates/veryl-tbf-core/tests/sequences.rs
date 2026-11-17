//! Sequencers, sequences and the driver's port, run through the exported
//! component with `veryl_component::testing::MockSim`.

use std::cell::RefCell;

use veryl_component::Value;
use veryl_component::testing::MockSim;
use veryl_tbf_core::prelude::*;
use veryl_tbf_core::{ArbRequest, Arbiter, Methodology, SeedSource};
use veryl_tbf_runtime::{Clock, TestError, join};

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(entry.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

#[derive(Clone, Debug, Default)]
struct Tx {
    tag: &'static str,
    addr: u32,
    read_data: u32,
}

struct Config {
    offset: u32,
}

#[component(sequencer(item = Tx))]
struct TxSequencer {
    #[context]
    context: InstanceContext<Config>,
}

/// Takes an item per edge; answers `addr + offset` in the item and as a
/// response.
#[component(driver)]
struct Driver {
    #[context]
    context: InstanceContext<Config>,
    clk: Clock,
    port: SeqItemPort<Tx>,
}

#[component(driver)]
impl Component for Driver {
    type Context = (InstanceContext<Config>, Clock);

    fn build(_scope: &Scope, (context, clk): Self::Context) -> Result<Self, TestError> {
        Ok(Driver {
            context,
            clk,
            port: SeqItemPort::new(),
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        loop {
            let item = self.port.get_next_item().await?;
            item.mark_begin();
            self.clk.edge().await;
            let data = {
                let mut it = item.borrow_mut();
                it.read_data = it.addr + self.context().config().offset;
                log(format!("drive {}{}", it.tag, it.addr));
                it.clone()
            };
            item.mark_end();
            self.port.put_response(&item, data)?;
            self.port.item_done()?;
        }
    }
}

/// The protocol's fields, and methods over them.
#[derive(Default)]
struct TxSeqBase {
    tag: &'static str,
}

trait TxSequence: SequenceCore<Sequencer = TxSequencer> + HasBase<TxSeqBase> {
    async fn read(&mut self, addr: u32) -> Result<ItemHandle<Tx>, TestError> {
        let mut item = Tx {
            tag: self.base().tag,
            addr,
            read_data: 0,
        };
        self.start_item(&mut item).await?;
        self.finish_item(item).await
    }
}

impl<S> TxSequence for S where S: SequenceCore<Sequencer = TxSequencer> + HasBase<TxSeqBase> {}

#[sequence(sequencer = TxSequencer, base = TxSeqBase)]
struct Reads {
    tag: &'static str,
    count: u32,
}

impl Sequence for Reads {
    async fn body(&mut self) -> Result<(), TestError> {
        self.base_mut().tag = self.tag;
        let offset = self.sequencer().context().config().offset;
        for addr in 0..self.count {
            let item = self.read(addr).await?;
            let response = self.get_response(&item).await;
            log(format!(
                "{} read {} = {} (response {}, offset {offset}, ended {})",
                self.tag,
                addr,
                item.borrow().read_data,
                response.read_data,
                item.ended()
            ));
        }
        Ok(())
    }
}

#[component(sequencer(item = ()))]
struct Vseqr {
    a: SequencerHandle<TxSequencer>,
    b: SequencerHandle<TxSequencer>,
}

#[sequence(sequencer = Vseqr)]
struct Both {
    count: u32,
}

impl Sequence for Both {
    async fn body(&mut self) -> Result<(), TestError> {
        let (a, b) = {
            let v = self.sequencer();
            (v.a.clone(), v.b.clone())
        };
        let mut ra = Reads::new("a", self.count);
        let mut rb = Reads::new("b", self.count);
        let (x, y) = join(ra.start_from(&a, self), rb.start_from(&b, self)).await;
        x?;
        y
    }
}

/// Grants the request of the highest priority.
struct ByPriority;

impl Arbiter for ByPriority {
    fn select(&mut self, requests: &[ArbRequest], _rng: &SeedSource) -> usize {
        let best = requests.iter().map(|r| r.priority).max().unwrap();
        requests.iter().position(|r| r.priority == best).unwrap()
    }
}

#[component(agent)]
struct Agent {
    sequencer: Instance<TxSequencer>,
    _driver: Instance<Driver>,
}

#[component(agent, passive)]
impl Component for Agent {
    type Context = (InstanceContext<Config>, Clock);

    fn build(scope: &Scope, (context, clk): Self::Context) -> Result<Self, TestError> {
        let sequencer = scope.create_instance::<TxSequencer>("sequencer", context.clone())?;
        let mut driver = scope.create_instance::<Driver>("driver", (context, clk))?;
        driver.port.connect(&sequencer.handle());
        Ok(Agent {
            sequencer,
            _driver: driver,
        })
    }
}

struct SeqTestConfig {
    clk: Clock,
    mode: &'static str,
}

#[component(test)]
struct SeqTest {
    a: Instance<Agent>,
    vseqr: Instance<Vseqr>,
    mode: &'static str,
}

#[component(test)]
impl Component for SeqTest {
    type Context = SeqTestConfig;

    fn build(scope: &Scope, config: SeqTestConfig) -> Result<Self, TestError> {
        let a = scope.create_instance::<Agent>(
            "a",
            (InstanceContext::new(Config { offset: 100 }), config.clk),
        )?;
        let b = scope.create_instance::<Agent>(
            "b",
            (InstanceContext::new(Config { offset: 200 }), config.clk),
        )?;
        let b_handle = if config.mode == "unbound" {
            SequencerHandle::default()
        } else {
            b.sequencer.handle()
        };
        let vseqr = scope.create_instance::<Vseqr>("vseqr", (a.sequencer.handle(), b_handle))?;
        if config.mode == "default" {
            b.sequencer
                .handle()
                .set_default_sequence(|| Reads::new("default", 2));
        }
        if config.mode == "priority" {
            a.sequencer.handle().set_arbiter(ByPriority);
        }
        Ok(SeqTest {
            a,
            vseqr,
            mode: config.mode,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        let a = self.a.borrow().sequencer.handle();
        match self.mode {
            "fifo" | "priority" => {
                let mut low = Reads::new("low", 2);
                let mut high = Reads::new("high", 2);
                high.set_priority(500);
                let (x, y) = join(low.start(&a), high.start(&a)).await;
                x?;
                y?;
            }
            _ => {
                let vseqr = self.vseqr.handle();
                Both::new(2).start(&vseqr).await?;
            }
        }
        Ok(())
    }
}

impl Test for SeqTest {
    fn setup(setup_context: &mut SetupContext) -> Result<SeqTestConfig, TestError> {
        Ok(SeqTestConfig {
            clk: veryl_tbf_runtime::host::registered_clock("clk")?,
            mode: match setup_context.test_name() {
                "Fifo" => "fifo",
                "Priority" => "priority",
                "Virtual" => "virtual",
                "Default" => "default",
                _ => "unbound",
            },
        })
    }
}

register_test!(Fifo, SeqTest, ());
register_test!(Priority, SeqTest, ());
register_test!(Virtual, SeqTest, ());
register_test!(Default, SeqTest, ());
register_test!(Unbound, SeqTest, ());

veryl_tbf_runtime::component!(Tb, Methodology);

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn run(test: &str, edges: usize) -> MockSim {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[str("clk"), clk]).unwrap();
    sim.call(&mut tb, "start", &[str(test)]).unwrap();
    sim.set_fired_clock("clk");
    for _ in 0..edges {
        sim.clock(&mut tb).unwrap();
    }
    sim
}

#[test]
fn fifo_arbitration_alternates_between_waiting_sequences() {
    let sim = run("Fifo", 4);
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    assert_eq!(
        take_log(),
        [
            "drive low0",
            "low read 0 = 100 (response 100, offset 100, ended true)",
            "drive high0",
            "high read 0 = 100 (response 100, offset 100, ended true)",
            "drive low1",
            "low read 1 = 101 (response 101, offset 100, ended true)",
            "drive high1",
            "high read 1 = 101 (response 101, offset 100, ended true)",
        ]
    );
}

#[test]
fn a_replaced_arbiter_grants_by_priority() {
    let sim = run("Priority", 4);
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    let drives: Vec<String> = take_log()
        .into_iter()
        .filter(|l| l.starts_with("drive"))
        .collect();
    assert_eq!(
        drives,
        ["drive high0", "drive high1", "drive low0", "drive low1"]
    );
}

#[test]
fn a_virtual_sequence_runs_sequences_on_its_sub_sequencers() {
    let sim = run("Virtual", 2);
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    let mut log = take_log();
    log.sort();
    assert_eq!(
        log,
        [
            "a read 0 = 100 (response 100, offset 100, ended true)",
            "a read 1 = 101 (response 101, offset 100, ended true)",
            "b read 0 = 200 (response 200, offset 200, ended true)",
            "b read 1 = 201 (response 201, offset 200, ended true)",
            "drive a0",
            "drive a1",
            "drive b0",
            "drive b1",
        ]
    );
}

#[test]
fn a_default_sequence_runs_in_the_background() {
    let sim = run("Default", 4);
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    let log = take_log();
    assert!(log.contains(&"drive default0".to_string()), "{log:?}");
    assert!(log.contains(&"drive default1".to_string()), "{log:?}");
}

#[test]
fn an_unbound_handle_fails_the_start() {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[str("clk"), clk]).unwrap();
    let error = sim.call(&mut tb, "start", &[str("Unbound")]).unwrap_err();
    assert!(
        error.to_string().contains("unbound handles: `vseqr.b`"),
        "{error}"
    );
}
