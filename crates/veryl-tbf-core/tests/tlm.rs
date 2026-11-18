//! Items, analysis ports and their receivers, run through the exported
//! component with `veryl_component::testing::MockSim`.

use std::cell::RefCell;
use std::rc::Rc;

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

#[derive(Clone, Debug, SequenceItem)]
struct Tx {
    addr: u32,
}

/// Publishes one transfer per two edges on `port`, marking its begin and
/// end; and the address alone on `addrs`.
struct Monitor {
    clk: Clock,
    port: AnalysisPort<TxHandle>,
    addrs: AnalysisPort<u32>,
}

impl Component for Monitor {
    type Context = Clock;

    fn build(_scope: &Scope, clk: Clock) -> Result<Self, TestError> {
        Ok(Monitor {
            clk,
            port: AnalysisPort::new(),
            addrs: AnalysisPort::new(),
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        let mut addr = 0;
        loop {
            addr += 1;
            self.clk.edge().await;
            let tx = Tx { addr }.into_handle();
            tx.mark_begin();
            self.port.write(&tx);
            self.clk.edge().await;
            tx.mark_end();
            self.addrs.write(&addr);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Tx,
    Addr,
}

/// Receives both ports; passive.
#[derive(Default)]
struct Board {
    got: Vec<String>,
}

impl Component for Board {
    type Context = ();
    const PASSIVE: bool = true;

    fn build(_scope: &Scope, (): ()) -> Result<Self, TestError> {
        Ok(Board::default())
    }

    fn check(&mut self, _check_context: &mut CheckContext) {
        log(format!("board {}", self.got.join(" ")));
    }
}

impl Subscriber<TxHandle> for Board {
    type Id = Side;
    fn write(&mut self, id: Side, item: &TxHandle) {
        self.got.push(format!("{id:?}{}", item.borrow().addr));
    }
}

impl Subscriber<u32> for Board {
    type Id = Side;
    fn write(&mut self, id: Side, addr: &u32) {
        self.got.push(format!("{id:?}{addr}"));
    }
}

/// Reads the monitor's transfers in its run, through a FIFO.
struct Collector {
    fifo: AnalysisFifo<TxHandle>,
    count: usize,
}

impl Component for Collector {
    type Context = ();

    fn build(_scope: &Scope, (): ()) -> Result<Self, TestError> {
        Ok(Collector {
            fifo: AnalysisFifo::new(),
            count: 0,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        loop {
            let tx = self.fifo.get().await;
            // The handle is shared: wait for the monitor to end the transfer.
            tx.end_event().wait_on().await;
            self.count += 1;
            log(format!(
                "collected {} ended {}",
                tx.borrow().addr,
                tx.ended()
            ));
        }
    }
}

#[component(test)]
struct TlmTest {
    monitor: Instance<Monitor>,
    waiter: ItemWaiter<TxHandle, u32>,
    seen: Rc<RefCell<Vec<u32>>>,
    collector: Instance<Collector>,
}

impl Component for TlmTest {
    type Context = Clock;

    fn build(scope: &Scope, clk: Clock) -> Result<Self, TestError> {
        let monitor = scope.create_instance::<Monitor>("monitor", clk)?;
        let board = scope.create_instance::<Board>("board", ())?;
        let collector = scope.create_instance::<Collector>("collector", ())?;
        monitor.port.connect(&board, Side::Tx);
        monitor.addrs.connect(&board, Side::Addr);
        monitor.port.connect_export(&collector.fifo.export());
        let seen = Rc::new(RefCell::new(Vec::new()));
        let model = seen.clone();
        monitor
            .addrs
            .subscribe(move |addr| model.borrow_mut().push(*addr));
        let waiter = ItemWaiter::new(|tx: &TxHandle| tx.borrow().addr);
        waiter.subscribe_to(&monitor.port);
        Ok(TlmTest {
            monitor,
            waiter,
            seen,
            collector,
        })
    }

    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        let first = self.waiter.next().await;
        log(format!("next {}", first.borrow().addr));
        let third = self.waiter.by_key(3).await;
        third.end_event().wait_on().await;
        log(format!("by key {} ended", third.borrow().addr));
        Ok(())
    }

    fn check(&mut self, _check_context: &mut CheckContext) {
        log(format!(
            "model {:?}, collector {}, monitor at {}",
            self.seen.borrow(),
            self.collector.count,
            self.monitor.clk.edges()
        ));
    }
}

impl Test for TlmTest {
    fn setup(_setup_context: &mut SetupContext) -> Result<Clock, TestError> {
        veryl_tbf_runtime::host::registered_clock("clk")
    }
}

/// Connects a port to a component with a run.
#[component(test)]
struct WrongTest;

impl Component for WrongTest {
    type Context = Clock;

    fn build(scope: &Scope, clk: Clock) -> Result<Self, TestError> {
        let monitor = scope.create_instance::<Monitor>("monitor", clk)?;
        let other = scope.create_instance::<Monitor>("other", clk)?;
        monitor.addrs.connect(&other, ());
        Ok(WrongTest)
    }
}

impl Subscriber<u32> for Monitor {
    type Id = ();
    fn write(&mut self, (): (), _addr: &u32) {}
}

impl Test for WrongTest {
    fn setup(_setup_context: &mut SetupContext) -> Result<Clock, TestError> {
        veryl_tbf_runtime::host::registered_clock("clk")
    }
}

register_test!(TlmTest);
register_test!(WrongTest);

veryl_tbf_runtime::component!(Tb, Methodology);

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn setup() -> (MockSim, Tb) {
    take_log();
    let mut sim = MockSim::new();
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[str("clk"), clk]).unwrap();
    (sim, tb)
}

#[test]
fn ports_reach_passive_components_closures_fifos_and_waiters() {
    let (mut sim, mut tb) = setup();
    sim.call(&mut tb, "start", &[str("TlmTest")]).unwrap();
    sim.set_fired_clock("clk");
    for _ in 0..6 {
        sim.clock(&mut tb).unwrap();
    }
    assert!(sim.finish_requested(), "{:?}", sim.failures());
    assert!(!sim.failed(), "{:?}", sim.failures());
    assert_eq!(
        take_log(),
        [
            "next 1",
            "collected 1 ended true",
            "collected 2 ended true",
            // Both wait for transfer 3's end; the collector waited first.
            "collected 3 ended true",
            "by key 3 ended",
            "board Tx1 Addr1 Tx2 Addr2 Tx3 Addr3",
            "model [1, 2, 3], collector 3, monitor at 6",
        ]
    );
}

#[test]
fn a_port_cannot_connect_to_a_component_with_a_run() {
    let (mut sim, mut tb) = setup();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sim.call(&mut tb, "start", &[str("WrongTest")])
    }))
    .unwrap_err();
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains("`other` has a run"), "{message}");
}

#[test]
fn an_item_event_asked_for_after_the_mark_is_already_on() {
    let tx = ItemHandle::new(Tx { addr: 9 });
    tx.mark_end();
    assert!(tx.end_event().is_on());
    assert!(!tx.begin_event().is_on());
    let shared = tx.clone();
    shared.borrow_mut().addr = 10;
    assert_eq!(tx.borrow().addr, 10);
    assert!(tx.ptr_eq(&shared));
}
