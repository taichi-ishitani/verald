//! A testbench for a register block on a simple valid/ready bus: a master
//! agent generated from its parts, a scoreboard holding a model of the
//! registers, and a test that writes every register and reads it back.

#![allow(async_fn_in_trait)]

use std::convert::Infallible;

use veryl_tbf::prelude::*;
use veryl_tbf::runtime::run_with_reset;

// ---------------------------------------------------------------------------
// The bus VIP
// ---------------------------------------------------------------------------

/// A transfer: a write of `data`, or a read whose `data` the driver (or the
/// monitor) fills in. Random under the agent's configuration, which bounds
/// the address.
#[randomize(context = BusConfig)]
#[derive(Clone, Debug, Default, SequenceItem)]
pub struct BusItem {
    #[rand]
    pub write: bool,
    #[rand]
    pub addr: u8,
    #[rand]
    pub data: u16,
}

#[constraints]
impl BusItem {
    fn in_range() {
        addr < context.regs;
    }
}

impl BusItem {
    pub fn new(write: bool, addr: u8, data: u16) -> Self {
        BusItem {
            write,
            addr,
            data,
            ..Default::default()
        }
    }
}

/// The agent's configuration: its mode, and the interfaces it uses, as the
/// test module registered them.
pub struct BusConfig {
    pub active: bool,
    /// The number of registers behind the bus.
    pub regs: u8,
    pub driver_if: &'static str,
    pub monitor_if: &'static str,
}

impl AgentConfig for BusConfig {
    fn active(&self) -> bool {
        self.active
    }
}

pub type BusContext = InstanceContext<BusConfig>;

/// The `master` modport, as the driver sees it.
#[modport]
pub struct BusMaster {
    #[output(init = false)]
    valid: bool,
    #[output]
    write: bool,
    #[output]
    addr: u8,
    #[output]
    wdata: u16,
    #[input]
    ready: bool,
    #[input]
    rvalid: bool,
    #[input]
    rdata: u16,
}

#[clocking]
pub struct DriverCb {
    #[modport]
    bus: BusMaster,
}

/// The `monitor` modport: everything is an input.
#[modport]
pub struct BusWatch {
    #[input]
    valid: bool,
    #[input]
    ready: bool,
    #[input]
    write: bool,
    #[input]
    addr: u8,
    #[input]
    wdata: u16,
    #[input]
    rvalid: bool,
    #[input]
    rdata: u16,
}

#[clocking]
pub struct MonitorCb {
    #[modport]
    bus: BusWatch,
}

/// Drives each item until the slave accepts it; a read waits for its data.
#[component(driver)]
pub struct BusDriver {
    #[context]
    context: BusContext,
    #[seq_item_port]
    port: SeqItemPort<BusItem>,
    cb: DriverCb,
    rst: Reset,
}

#[component(driver)]
impl Component for BusDriver {
    type Context = BusContext;

    fn build(scope: &Scope, context: BusContext) -> Result<Self, TestError> {
        let cb = scope.clocking_with(
            "clk",
            DriverCbPaths {
                bus: Some(context.config().driver_if),
                ..Default::default()
            },
        )?;
        Ok(BusDriver {
            rst: scope.reset("rst")?,
            context,
            port: SeqItemPort::new(),
            cb,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        let rst = self.rst;
        let never: Result<Infallible, TestError> = run_with_reset(
            rst,
            self,
            |driver| driver.cb.init(),
            async |driver| {
                loop {
                    driver.drive_one().await?;
                }
            },
        )
        .await;
        never.map(|_| ())
    }
}

impl BusDriver {
    async fn drive_one(&mut self) -> Result<(), TestError> {
        let item = self.port.get_next_item().await?;
        let request = item.borrow().clone();
        let cb = &mut self.cb;
        cb.bus.valid = true;
        cb.bus.write = request.write;
        cb.bus.addr = request.addr;
        cb.bus.wdata = request.data;
        item.mark_begin();
        // Accepted at the edge where `ready` is high with `valid`.
        tick!(cb);
        while !cb.bus.ready {
            tick!(cb);
        }
        cb.bus.valid = false;
        if !request.write {
            tick_until!(cb.bus.rvalid);
            item.borrow_mut().data = cb.bus.rdata;
        }
        item.mark_end();
        self.port.item_done()
    }
}

/// Publishes every transfer the slave accepts: a write at its acceptance, a
/// read when its data comes back. One request is outstanding at a time, as
/// the master sends them.
#[component(monitor)]
pub struct BusMonitor {
    #[context]
    context: BusContext,
    #[analysis_port]
    port: AnalysisPort<BusItemHandle>,
    cb: MonitorCb,
}

#[component(monitor)]
impl Component for BusMonitor {
    type Context = BusContext;

    fn build(scope: &Scope, context: BusContext) -> Result<Self, TestError> {
        let cb = scope.clocking_with(
            "clk",
            MonitorCbPaths {
                bus: Some(context.config().monitor_if),
                ..Default::default()
            },
        )?;
        Ok(BusMonitor {
            context,
            port: AnalysisPort::new(),
            cb,
        })
    }

    async fn run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> {
        let cb = &mut self.cb;
        loop {
            // A request, until the slave accepts it: at an edge where
            // `ready` is high with `valid`.
            tick_until!(cb.bus.valid);
            if !cb.bus.ready {
                tick_until!(cb.bus.ready);
            }
            let (write, addr, wdata) = (cb.bus.write, cb.bus.addr, cb.bus.wdata);
            let item = if write {
                run_context.trace(format!("write {addr} = {wdata:#06x}"));
                BusItem::new(true, addr, wdata).into_handle()
            } else {
                // The data comes back with `rvalid`; the master sends the
                // next request only after it.
                tick_until!(cb.bus.rvalid);
                let rdata = cb.bus.rdata;
                run_context.trace(format!("read {addr} = {rdata:#06x}"));
                BusItem::new(false, addr, rdata).into_handle()
            };
            self.port.write(&item);
        }
    }
}

#[component(sequencer(item = BusItem))]
pub struct BusSequencer {
    #[context]
    context: BusContext,
}

/// The master agent, made from its parts.
#[component(agent(monitor = BusMonitor, sequencer = BusSequencer, driver = BusDriver))]
pub struct BusAgent {}

/// What every bus sequence can do.
#[derive(Default)]
pub struct BusSeqBase {}

pub trait BusSequence: SequenceCore<Sequencer = BusSequencer> + HasBase<BusSeqBase> {
    async fn write(&mut self, addr: u8, data: u16) -> Result<(), TestError> {
        let mut item = BusItem::new(true, addr, data);
        self.start_item(&mut item).await?;
        self.finish_item(item).await?;
        Ok(())
    }

    async fn read(&mut self, addr: u8) -> Result<u16, TestError> {
        let mut item = BusItem::new(false, addr, 0);
        self.start_item(&mut item).await?;
        let item = self.finish_item(item).await?;
        let data = item.borrow().data;
        Ok(data)
    }
}

impl<S> BusSequence for S where S: SequenceCore<Sequencer = BusSequencer> + HasBase<BusSeqBase> {}

// ---------------------------------------------------------------------------
// The testbench
// ---------------------------------------------------------------------------

/// Checks every read against a model of the registers, which the writes
/// keep up to date.
#[component(scoreboard)]
pub struct RegScoreboard {
    model: [u16; 8],
    reads: usize,
    writes: usize,
    mismatches: Vec<String>,
}

#[component(scoreboard, passive)]
impl Component for RegScoreboard {
    type Context = ();

    fn build(_scope: &Scope, (): ()) -> Result<Self, TestError> {
        Ok(RegScoreboard {
            model: [0; 8],
            reads: 0,
            writes: 0,
            mismatches: Vec::new(),
        })
    }

    fn check(&mut self, check_context: &mut CheckContext) {
        for mismatch in &self.mismatches {
            check_context.error(mismatch);
        }
        if self.reads == 0 {
            check_context.error("no read was checked");
        }
    }

    fn report(&self, report_context: &ReportContext) {
        report_context.info(format!(
            "{} writes, {} reads checked, {} mismatches",
            self.writes,
            self.reads,
            self.mismatches.len()
        ));
    }
}

impl Subscriber<BusItemHandle> for RegScoreboard {
    type Id = ();

    fn write(&mut self, (): (), item: &BusItemHandle) {
        let item = item.borrow();
        let reg = &mut self.model[usize::from(item.addr)];
        if item.write {
            *reg = item.data;
            self.writes += 1;
        } else {
            self.reads += 1;
            if item.data != *reg {
                self.mismatches.push(format!(
                    "read {}: expected {:#06x}, got {:#06x}",
                    item.addr, *reg, item.data
                ));
            }
        }
    }
}

#[component(env)]
pub struct RegEnv {
    pub bus: Instance<BusAgent>,
    pub scoreboard: Instance<RegScoreboard>,
}

#[component(env, passive)]
impl Component for RegEnv {
    type Context = BusContext;

    fn build(scope: &Scope, context: BusContext) -> Result<Self, TestError> {
        let bus = scope.create_instance::<BusAgent>("bus", context)?;
        let scoreboard = scope.create_instance::<RegScoreboard>("scoreboard", ())?;
        bus.analysis_port().connect(&scoreboard, ());
        Ok(RegEnv { bus, scoreboard })
    }
}

/// Writes every register, then reads every one back, `rounds` times with
/// different data.
#[sequence(sequencer = BusSequencer, base = BusSeqBase)]
pub struct WriteReadAll {
    rounds: u16,
}

impl Sequence for WriteReadAll {
    async fn body(&mut self) -> Result<(), TestError> {
        for round in 0..self.rounds {
            for addr in 0..8u8 {
                let data = (0x1111 * u16::from(addr)) ^ (round << 8) ^ 0x00a5;
                self.write(addr, data).await?;
            }
            for addr in 0..8u8 {
                self.read(addr).await?;
            }
        }
        Ok(())
    }
}

pub struct RegTestConfig {
    pub rounds: u16,
}

/// The base of the tests: the configuration, the hierarchy, and what every
/// test does before and after its scenario. It has no scenario of its own;
/// the tests below are made from it.
#[component(test, base)]
pub struct RegTest {
    env: Instance<RegEnv>,
    sequencer: SequencerHandle<BusSequencer>,
    rst: Reset,
    rounds: u16,
}

#[component(test, base)]
impl Component for RegTest {
    type Context = RegTestConfig;

    fn setup(setup_context: &mut SetupContext) -> Result<RegTestConfig, TestError> {
        Ok(RegTestConfig {
            rounds: setup_context.get("rounds")?,
        })
    }

    fn build(scope: &Scope, config: RegTestConfig) -> Result<Self, TestError> {
        let context = BusConfig {
            active: true,
            regs: 8,
            driver_if: "bus",
            monitor_if: "bus_mon",
        }
        .into_context();
        let env = scope.create_instance::<RegEnv>("env", context)?;
        // Taken from the child right after making it.
        let sequencer = env.bus.sequencer_handle();
        Ok(RegTest {
            env,
            sequencer,
            rst: scope.reset("rst")?,
            rounds: config.rounds,
        })
    }

    /// Every scenario starts after the reset.
    async fn pre_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        self.rst.asserted().await;
        self.rst.released().await;
        Ok(())
    }

    /// Every scenario ends by telling what the scoreboard saw.
    async fn post_run(&mut self, run_context: &mut RunContext) -> Result<(), TestError> {
        run_context.info(format!(
            "done; the scoreboard has seen {} reads",
            self.env.borrow().scoreboard.borrow().reads
        ));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The tests, made from RegTest: one writing its `run` itself, the others
// with a scenario each
// ---------------------------------------------------------------------------

/// Runs `WriteReadAll`, its `run` written as a method; RegTest's fields are
/// reached through it.
#[component(test(base = RegTest, run))]
pub struct BasicTest {}

impl BasicTest {
    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        WriteReadAll::new(self.rounds).start(&self.sequencer).await
    }
}

register_test!(BasicTest);

/// Writes every register, then reads them back from the last to the first,
/// interleaving a rewrite of each register after its read.
#[sequence(sequencer = BusSequencer, base = BusSeqBase)]
pub struct ReverseReadBack {}

impl Sequence for ReverseReadBack {
    async fn body(&mut self) -> Result<(), TestError> {
        for addr in 0..8u8 {
            self.write(addr, 0xa000 | u16::from(addr)).await?;
        }
        for addr in (0..8u8).rev() {
            self.read(addr).await?;
            self.write(addr, 0x5000 | u16::from(addr)).await?;
            self.read(addr).await?;
        }
        Ok(())
    }
}

/// Runs `ReverseReadBack` on RegTest's sequencer.
#[derive(Default)]
pub struct ReverseScenario {}

impl Scenario for ReverseScenario {
    type Test = RegTest;

    async fn body(
        &mut self,
        test: &mut RegTest,
        run_context: &mut RunContext,
    ) -> Result<(), TestError> {
        run_context.info("reading back in reverse");
        ReverseReadBack::new().start(&test.sequencer).await
    }
}

#[component(test(base = RegTest, run = ReverseScenario))]
pub struct ReverseTest {}

register_test!(ReverseTest);

/// Random writes and reads, `count` of them, each randomized under the
/// agent's configuration (so within its registers).
#[sequence(sequencer = BusSequencer, base = BusSeqBase)]
pub struct RandomAccess {
    count: u16,
}

impl Sequence for RandomAccess {
    async fn body(&mut self) -> Result<(), TestError> {
        for _ in 0..self.count {
            let mut item = BusItem::default();
            self.start_item(&mut item).await?;
            self.randomize_item(&mut item)?;
            self.finish_item(item).await?;
        }
        Ok(())
    }
}

/// Runs `RandomAccess` on RegTest's sequencer, 24 transfers a round.
#[derive(Default)]
pub struct RandomScenario {}

impl Scenario for RandomScenario {
    type Test = RegTest;

    async fn body(
        &mut self,
        test: &mut RegTest,
        run_context: &mut RunContext,
    ) -> Result<(), TestError> {
        let count = 24 * test.rounds;
        run_context.info(format!("{count} random transfers"));
        RandomAccess::new(count).start(&test.sequencer).await
    }
}

#[component(test(base = RegTest, run = RandomScenario))]
pub struct RandomTest {}

register_test!(RandomTest);

veryl_tbf::export!("tbf_regblock");
