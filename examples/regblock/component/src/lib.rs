//! A testbench for a register block on a simple valid/ready bus, over the
//! bus's VIP (`regblock_vip`): a scoreboard holding a model of the
//! registers, an environment, and tests that write every register and read
//! it back. The bus's assertions (`regblock_assertions`) run beside it.

#![allow(async_fn_in_trait)]

use regblock_assertions::BusAssertions;
use regblock_vip::*;
use verald::prelude::*;

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

    fn setup(_setup_context: &mut SetupContext) -> Result<RegTestConfig, TestError> {
        Ok(RegTestConfig {
            rounds: runtime_arg_value("ROUNDS")?.unwrap_or(2),
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

// ---------------------------------------------------------------------------
// A test written as a function, calling the VIP's methods as tasks
// ---------------------------------------------------------------------------

/// Writes a few registers and reads them back one by one, checking each
/// read where it is made, as a scenario of task calls on the VIP's
/// `BusBfm`: `BusSequence`'s `write` and `read`, through the sequencer.
#[testcase(base = RegTest)]
async fn direct_test(test: &mut RegTest, run_context: &mut RunContext) -> Result<(), TestError> {
    let mut bus = BusBfm::new(&test.sequencer);
    for addr in 0..4u8 {
        bus.write(addr, 0x0f00 | u16::from(addr)).await?;
    }
    for addr in 0..4u8 {
        let data = bus.read(addr).await?;
        if data != 0x0f00 | u16::from(addr) {
            run_context.error(format!("read {addr}: got {data:#06x}"));
        }
    }
    Ok(())
}

// The testbench and the bus's assertions (`regblock_assertions`) in one component.
verald::export!(
    "verald_regblock",
    testbench = Methodology,
    assertions = [BusAssertions]
);
