//! The runtime's state, one per simulation, in thread-local storage: a
//! Veryl test runs all its hooks on one thread, and the testbench uses `Rc`,
//! which a Veryl component (`Send`) cannot hold.

use std::cell::RefCell;

use veryl_component::Value;

use crate::TestError;
use crate::executor::TaskId;
use crate::signals::{ClockState, ResetState};

/// Where the simulation is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    /// Before `start`: the test module registers clocks, resets and
    /// interfaces (with the host layer) and gives options and settings.
    Setup,
    /// From `start` until the main task returns or an error ends the run.
    Running,
    /// After the end of the run phase.
    Ended,
}

pub(crate) struct Setting {
    pub(crate) key: String,
    pub(crate) value: Value,
    pub(crate) read: bool,
}

/// How the run phase ended, as handed to the end handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// The main task returned.
    Finished,
    /// A fatal error ended the run phase.
    Fatal { owner: String, error: TestError },
}

pub(crate) type EndHandler = Box<dyn FnOnce(&RunEnd) -> Result<(), TestError>>;

/// The timeout's default, in edges of the master clock: beyond any usual
/// test, so that a hung one still ends, as UVM's default timeout.
pub(crate) const DEFAULT_TIMEOUT: u64 = 10_000_000;

pub(crate) struct State {
    pub(crate) phase: Phase,
    /// The clocks and resets the test module registered, built from the
    /// host layer's registrations at `start`, in the order registered.
    pub(crate) clocks: Vec<ClockState>,
    pub(crate) resets: Vec<ResetState>,
    pub(crate) settings: Vec<Setting>,
    /// The edges of the master clock the run phase must end within; 0 for
    /// no limit.
    pub(crate) timeout: u64,
    /// The clock `set_master_clock` named, resolved at `start`.
    pub(crate) master_clock_name: Option<String>,
    /// The clock the timeout counts (an index into `clocks`); the first
    /// registered unless `set_master_clock` names another.
    pub(crate) master_clock: Option<usize>,
    pub(crate) main: Option<TaskId>,
    pub(crate) main_done: bool,
    pub(crate) on_end: Option<EndHandler>,
    /// A fatal error raised outside a task's return (`testbench::fatal`), ending
    /// the run phase once the running task yields.
    pub(crate) pending_fatal: Option<(String, TestError)>,
}

impl State {
    fn new() -> Self {
        State {
            phase: Phase::Setup,
            clocks: Vec::new(),
            resets: Vec::new(),
            settings: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            master_clock_name: None,
            master_clock: None,
            main: None,
            main_done: false,
            on_end: None,
            pending_fatal: None,
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Starts a fresh state, dropping any left by an earlier simulation on this
/// thread.
pub(crate) fn init() {
    clear();
    STATE.with(|s| *s.borrow_mut() = Some(State::new()));
}

/// Drops every task, then the state.
pub(crate) fn clear() {
    crate::executor::reset();
    let old = STATE.with(|s| s.borrow_mut().take());
    drop(old);
}

pub(crate) fn exists() -> bool {
    STATE.with(|s| s.borrow().is_some())
}

/// Runs `f` on the state. Panics outside a simulation: clocks and resets
/// are only reachable while one runs.
pub(crate) fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let state = s
            .as_mut()
            .expect("the Verald runtime is used outside a simulation");
        f(state)
    })
}
