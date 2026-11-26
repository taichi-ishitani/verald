//! A VIP for the register block's valid/ready bus: the item, the agent's
//! configuration, the modports and clocking blocks, the driver, the
//! monitor, the sequencer and the agent, and a base for sequences. Shared
//! by the testbench's component; the bus's assertions use its monitor
//! modport.

#![allow(async_fn_in_trait)]

use std::convert::Infallible;

use verald::prelude::*;
use verald::tb::runtime::run_with_reset;

/// A transfer: a write of `data`, or a read whose `data` the driver (or the
/// monitor) fills in. Random under the agent's configuration, which bounds
/// the address.
#[randomize(context = BusConfig)]
#[sequence_item]
#[derive(Clone, Debug, Default)]
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
    #[clock]
    clk: Clock,
    #[modport]
    bus: BusMaster,
}

/// The `monitor` modport: everything is an input.
#[modport]
pub struct BusWatch {
    #[input]
    pub valid: bool,
    #[input]
    pub ready: bool,
    #[input]
    pub write: bool,
    #[input]
    pub addr: u8,
    #[input]
    pub wdata: u16,
    #[input]
    pub rvalid: bool,
    #[input]
    pub rdata: u16,
}

#[clocking]
pub struct MonitorCb {
    #[clock]
    clk: Clock,
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
        let cb = scope.clocking_with(DriverCbPaths {
            bus: Some(context.config().driver_if),
            ..Default::default()
        })?;
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
        let cb = scope.clocking_with(MonitorCbPaths {
            bus: Some(context.config().monitor_if),
            ..Default::default()
        })?;
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

/// The bus as a BFM: a sequence with no body, on which a test calls
/// `BusSequence`'s `write` and `read` as tasks.
#[sequence(sequencer = BusSequencer, base = BusSeqBase, proxy)]
pub struct BusBfm {}
