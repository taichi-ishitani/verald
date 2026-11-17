//! Veryl's simulator adapter: the hooks and methods of the exported
//! testbench component, and what they do to the runtime.
//!
//! The component holds nothing; the runtime's state is thread-local (see
//! `state`). The test module registers clocks, resets and interfaces, gives
//! options and settings, then calls `start`, as UVM's `run_test`:
//!
//! ```veryl
//! inst tb: $comp::tb;
//! initial {
//!     tb.set_clock("clk", clk);
//!     tb.set_reset("rst", rst);
//!     tb.set_interface("bus", bus.tb);
//!     tb.option("timeout", 100000);
//!     tb.start("my_test");
//!     clk.next(1000000);
//!     $finish();
//! }
//! ```

use std::future::Future;
use std::rc::Rc;

use veryl_component::{ClockPort, MemberPort, ModportRef, ResetPort, SimCtx, Value, bail};

use crate::TestError;
use crate::executor::{self, Polled};
use crate::signals::{Clock, ClockState, Reset, ResetState};
use crate::sim::{self, HookGuard};
use crate::state::{self, EndHandler, Phase, RunEnd, Setting, with_state};

type HookResult = veryl_component::Result<()>;

/// What a testbench gives the runtime: how to take options, and how to start
/// a test. `veryl-tbf`'s methodology implements it; a testbench on the
/// runtime alone may too.
pub trait Testbench: 'static {
    /// Takes an option the runtime does not know (`tb.option(key, value)`).
    /// Returns `Ok(false)` for a key it does not know either.
    fn option(_key: &str, _value: &Value) -> Result<bool, TestError> {
        Ok(false)
    }

    /// Starts the test named by the test module (`tb.start(name)`): spawns
    /// its tasks and sets the main task, whose end ends the run phase.
    fn start(start: &mut Start) -> Result<(), TestError>;
}

/// The options the runtime takes itself.
const RUNTIME_OPTIONS: &[(&str, &str)] = &[("timeout", "u64: the time the test must end by")];

/// What `Testbench::start` builds the test with.
pub struct Start {
    test: String,
    main: bool,
}

impl Start {
    /// The test's name, as given to `start`.
    pub fn test_name(&self) -> &str {
        &self.test
    }

    /// The clock registered as `name`.
    pub fn clock(&self, name: &str) -> Result<Clock, TestError> {
        with_state(|s| s.clocks.iter().position(|c| c.name == name))
            .map(Clock::new)
            .ok_or_else(|| TestError::new(format!("no clock is registered as `{name}`")))
    }

    /// The reset registered as `name`.
    pub fn reset(&self, name: &str) -> Result<Reset, TestError> {
        with_state(|s| s.resets.iter().position(|r| r.name == name))
            .map(Reset::new)
            .ok_or_else(|| TestError::new(format!("no reset is registered as `{name}`")))
    }

    /// The interface registered as `name`.
    pub fn interface(&self, name: &str) -> Result<ModportRef, TestError> {
        with_state(|s| {
            s.interfaces
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, bus)| bus.clone())
        })
        .ok_or_else(|| TestError::new(format!("no interface is registered as `{name}`")))
    }

    /// The setting the test module gave as `key` (`tb.set(key, value)`),
    /// if any. A setting no one reads fails the start.
    pub fn setting(&self, key: &str) -> Option<Value> {
        with_state(|s| {
            let setting = s.settings.iter_mut().find(|setting| setting.key == key)?;
            setting.read = true;
            Some(setting.value.clone())
        })
    }

    /// Spawns a task for the run phase; `owner` names it in messages. It is
    /// dropped when the run phase ends, if still going.
    pub fn spawn(
        &mut self,
        owner: &str,
        task: impl Future<Output = Result<(), TestError>> + 'static,
    ) {
        executor::spawn(Rc::from(owner), Box::pin(task));
    }

    /// Spawns the main task: the run phase ends when it returns.
    pub fn main(
        &mut self,
        owner: &str,
        task: impl Future<Output = Result<(), TestError>> + 'static,
    ) {
        let id = executor::spawn(Rc::from(owner), Box::pin(task));
        with_state(|s| s.main = Some(id));
        self.main = true;
    }

    /// Sets what runs when the run phase has ended and every task is
    /// dropped (the methodology's `check` and `report`). An `Err` fails the
    /// test.
    pub fn on_end(&mut self, handler: impl FnOnce(&RunEnd) -> Result<(), TestError> + 'static) {
        let handler: EndHandler = Box::new(handler);
        with_state(|s| s.on_end = Some(handler));
    }
}

/// Enters a hook: publishes its context and checks for a poisoned runtime.
/// `None` when the hook has nothing to do.
fn enter(ctx: &mut SimCtx) -> Option<HookGuard> {
    if !state::exists() {
        return None;
    }
    let poisoned = with_state(|s| {
        if s.in_hook {
            // The previous hook unwound: a panic, reported by Veryl.
            s.poisoned = true;
        }
        s.in_hook = true;
        s.poisoned
    });
    if poisoned {
        return None;
    }
    Some(HookGuard::enter(ctx))
}

/// Leaves a hook normally.
fn leave() {
    if state::exists() {
        with_state(|s| s.in_hook = false);
    }
}

fn wake_all(wakers: Vec<std::task::Waker>) {
    for waker in wakers {
        waker.wake();
    }
}

/// Polls the ready tasks, then ends the run phase if the main task has
/// returned or a task has failed.
fn run() {
    if with_state(|s| s.phase) != Phase::Running {
        return;
    }
    let polled = executor::run_ready(|id| {
        with_state(|s| {
            if s.main == Some(id) {
                s.main_done = true;
            }
        })
    });
    match polled {
        Polled::Fatal { owner, error } => end(RunEnd::Fatal {
            owner: owner.to_string(),
            error,
        }),
        Polled::Idle => {
            if with_state(|s| s.main_done) {
                end(RunEnd::Finished);
            }
        }
    }
}

/// Ends the run phase: drops the remaining tasks, runs the end handler, and
/// finishes or fails the Veryl test (at the end of the cycle).
fn end(how: RunEnd) {
    with_state(|s| s.phase = Phase::Ended);
    executor::drop_all();
    let handler = with_state(|s| s.on_end.take());
    let handled = handler.map_or(Ok(()), |handler| handler(&how));
    match (&how, handled) {
        (RunEnd::Fatal { owner, error }, _) => sim::fail(&fatal_message(owner, error)),
        (RunEnd::Finished, Err(error)) => sim::fail(error.message()),
        (RunEnd::Finished, Ok(())) => sim::finish(),
    }
}

fn fatal_message(owner: &str, error: &TestError) -> String {
    if owner.is_empty() {
        format!("fatal: {error}")
    } else {
        format!("fatal: {owner}: {error}")
    }
}

/// The tasks still alive, for a timeout or an unexpected finish.
fn alive() -> String {
    let owners = executor::alive_owners();
    if owners.is_empty() {
        "no task alive".to_string()
    } else {
        let names: Vec<&str> = owners.iter().map(|o| &**o).collect();
        format!("tasks alive: {}", names.join(", "))
    }
}

fn check_timeout() {
    let Some(deadline) = with_state(|s| s.timeout) else {
        return;
    };
    let now = sim::time();
    if now >= deadline && with_state(|s| s.phase) == Phase::Running {
        let error = TestError::new(format!("timeout at time {now}; {}", alive()));
        end(RunEnd::Fatal {
            owner: String::new(),
            error,
        });
    }
}

/// `on_init`: a fresh runtime for this simulation.
pub fn on_init(_ctx: &mut SimCtx) -> HookResult {
    state::init();
    Ok(())
}

/// `on_clock`: counts the edge of the clock that fired, wakes its waiters,
/// releases the resets (`on_clock` and `on_reset` are exclusive: a reset in
/// force calls `on_reset` at every edge), and runs the ready tasks.
pub fn on_clock(ctx: &mut SimCtx) -> HookResult {
    let Some(_guard) = enter(ctx) else {
        return Ok(());
    };
    if with_state(|s| s.phase) == Phase::Running {
        check_timeout();
        let fired: Vec<usize> = with_state(|s| {
            s.clocks
                .iter()
                .enumerate()
                .filter(|(_, c)| sim::fired(c.port))
                .map(|(i, _)| i)
                .collect()
        });
        for i in fired {
            let wakers = with_state(|s| s.clocks[i].edge());
            let mut list = wakers;
            for waker in list.drain(..) {
                waker.wake();
            }
            with_state(|s| s.clocks[i].recycle(list));
        }
        let released: Vec<std::task::Waker> =
            with_state(|s| s.resets.iter_mut().flat_map(|r| r.set(false)).collect());
        wake_all(released);
        run();
    }
    leave();
    Ok(())
}

/// `on_reset`: asserts the reset that fired and runs the ready tasks.
pub fn on_reset(ctx: &mut SimCtx) -> HookResult {
    let Some(_guard) = enter(ctx) else {
        return Ok(());
    };
    if with_state(|s| s.phase) == Phase::Running {
        check_timeout();
        let asserted: Vec<std::task::Waker> = with_state(|s| {
            s.resets
                .iter_mut()
                .filter(|r| sim::fired(r.port))
                .flat_map(|r| r.set(true))
                .collect()
        });
        wake_all(asserted);
        run();
    }
    leave();
    Ok(())
}

/// `on_finish`: an end before the run phase's is an unexpected finish.
pub fn on_finish(ctx: &mut SimCtx) -> HookResult {
    if let Some(_guard) = enter(ctx) {
        match with_state(|s| s.phase) {
            Phase::Setup => ctx_fail("the testbench was not started: call `start`"),
            Phase::Running => ctx_fail(&format!(
                "unexpected finish: the simulation ended before the test did \
                 (the test module's `$finish` or cycle limit?); {}",
                alive()
            )),
            Phase::Ended => {}
        }
    }
    state::clear();
    Ok(())
}

fn ctx_fail(message: &str) {
    sim::fail(message);
}

fn in_setup(what: &str) -> veryl_component::Result<()> {
    if !state::exists() {
        bail!("the testbench got `{what}` before `on_init`");
    }
    if with_state(|s| s.phase) != Phase::Setup {
        bail!("`{what}` is only allowed before `start`");
    }
    Ok(())
}

fn check_name(name: &str) -> veryl_component::Result<()> {
    if name.is_empty() {
        bail!("an empty name cannot be registered");
    }
    if with_state(|s| s.has_name(name)) {
        bail!("`{name}` is already registered");
    }
    Ok(())
}

/// `set_clock(name, clk)`: registers a clock.
pub fn set_clock(_ctx: &mut SimCtx, name: String, clk: ClockPort) -> HookResult {
    in_setup("set_clock")?;
    check_name(&name)?;
    with_state(|s| s.clocks.push(ClockState::new(name, clk)));
    Ok(())
}

/// `set_reset(name, rst)`: registers a reset.
pub fn set_reset(_ctx: &mut SimCtx, name: String, rst: ResetPort) -> HookResult {
    in_setup("set_reset")?;
    check_name(&name)?;
    with_state(|s| s.resets.push(ResetState::new(name, rst)));
    Ok(())
}

/// `set_interface(name, bus.modport)`: registers an interface; its clock and
/// reset members are registered as `<name>.<member>`.
pub fn set_interface(_ctx: &mut SimCtx, name: String, bus: ModportRef) -> HookResult {
    in_setup("set_interface")?;
    check_name(&name)?;
    for member in bus.members() {
        let path = format!("{name}.{}", member.name);
        match member.port {
            MemberPort::Clock(port) => {
                check_name(&path)?;
                with_state(|s| s.clocks.push(ClockState::new(path, port)));
            }
            MemberPort::Reset(port) => {
                check_name(&path)?;
                with_state(|s| s.resets.push(ResetState::new(path, port)));
            }
            MemberPort::Input(_) | MemberPort::Output(_) => {}
        }
    }
    with_state(|s| s.interfaces.push((name, bus)));
    Ok(())
}

/// `option(key, value)`: an option of the runtime or the testbench, checked
/// at once.
pub fn option<T: Testbench>(_ctx: &mut SimCtx, key: String, value: Value) -> HookResult {
    in_setup("option")?;
    if key == "timeout" {
        let time = value
            .as_u64()
            .map_err(|e| veryl_component::anyhow!("option `timeout`: {e}"))?;
        with_state(|s| s.timeout = Some(time));
        return Ok(());
    }
    match T::option(&key, &value) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let known: Vec<String> = RUNTIME_OPTIONS
                .iter()
                .map(|(k, desc)| format!("`{k}` ({desc})"))
                .collect();
            bail!(
                "unknown option `{key}`; the runtime's: {}",
                known.join(", ")
            )
        }
        Err(e) => bail!("option `{key}`: {e}"),
    }
}

/// `set(key, value)`: a setting for the test, read when it starts.
pub fn set(_ctx: &mut SimCtx, key: String, value: Value) -> HookResult {
    in_setup("set")?;
    if with_state(|s| s.settings.iter().any(|setting| setting.key == key)) {
        bail!("setting `{key}` is given twice");
    }
    with_state(|s| {
        s.settings.push(Setting {
            key,
            value,
            read: false,
        })
    });
    Ok(())
}

/// `start(test)`: starts the test, as UVM's `run_test`, and runs its tasks
/// up to their first waits (time 0).
pub fn start<T: Testbench>(ctx: &mut SimCtx, test: String) -> HookResult {
    in_setup("start")?;
    let Some(_guard) = enter(ctx) else {
        return Ok(());
    };
    let mut start = Start { test, main: false };
    let started = T::start(&mut start);
    let result = started.and_then(|()| {
        if !start.main {
            return Err(TestError::new("the testbench set no main task"));
        }
        let unread: Vec<String> = with_state(|s| {
            s.settings
                .iter()
                .filter(|setting| !setting.read)
                .map(|setting| format!("`{}`", setting.key))
                .collect()
        });
        if unread.is_empty() {
            Ok(())
        } else {
            Err(TestError::new(format!(
                "settings no one read (misspelt?): {}",
                unread.join(", ")
            )))
        }
    });
    match result {
        Ok(()) => {
            with_state(|s| s.phase = Phase::Running);
            run();
            leave();
            Ok(())
        }
        Err(error) => {
            with_state(|s| s.phase = Phase::Ended);
            executor::drop_all();
            leave();
            bail!("test `{}`: {error}", start.test)
        }
    }
}

/// The component the testbench exports to Veryl; see [`crate::export!`].
#[derive(Default)]
pub struct Host;
