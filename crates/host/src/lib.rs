//! The host layer: what talks to Veryl's host API (veryl-component), shared
//! by the layers built on it (the runtime, and later the assertions). See
//! `docs/host-layer-design.md`.
//!
//! The layer also keeps the test module's registrations (`registry`) and
//! its `start`, which closes them, and generates a component over its
//! subscribers (`component!`).
//!
//! Signals are read and written only within a hook, with clocking-block
//! semantics: a read returns the value before the edge, a write reaches the
//! design at the edge. The `SimCtx` of the hook being run is published here
//! for its duration.

mod component;
mod registry;
mod signal;

use std::cell::{Cell, RefCell};
use std::fmt;

pub use veryl_component::{
    ArgError, ArgValue, BuildCtx, ClockPort, InputPort, MemberPort, ModportRef, OutputPort,
    ResetPort, RuntimeArg, SimCtx, Value,
};

pub use registry::Registered;
pub use signal::{
    InputOnly, InputSignal, MemberSource, ModportRead, OutputSignal, SignalValue, Signals,
};

/// The test module's registrations, closed by `start`.
pub mod registrations {
    pub use super::registry::{
        Registered, clock, clocks, input, instance_paths, modport, output, reset, resets,
        set_clock, set_input, set_modport, set_output, set_reset, started,
    };
}

/// A layer the hooks are dispatched to: the runtime, later the assertions.
///
/// A component holds its subscribers as fields of their own types, built
/// with `Default` and then given `on_build`, as veryl-component builds a
/// component's plain state; each hook calls them in turn. The hooks after
/// `on_build` take no context: the layer publishes it for the hook's
/// duration, and the subscribers use the services.
pub trait HookSubscriber: Default {
    /// At `create`: ports and parameters are resolved here.
    fn on_build(&mut self, _ctx: &mut BuildCtx) -> Result<(), HostError> {
        Ok(())
    }
    /// At the start of a simulation.
    fn on_init(&mut self) -> Result<(), HostError> {
        Ok(())
    }
    /// At the test module's `start`: the registrations are complete.
    /// `test` is the test's name where the component takes one (with the
    /// testbench), `None` otherwise.
    fn on_start(&mut self, _test: Option<&str>) -> Result<(), HostError> {
        Ok(())
    }
    /// At an edge of a clock the component listens to; `fired` tells which.
    fn on_clock(&mut self) -> Result<(), HostError> {
        Ok(())
    }
    /// At an edge while a reset is in force; `fired` tells which.
    fn on_reset(&mut self) -> Result<(), HostError> {
        Ok(())
    }
    /// At the end of the simulation. Called even on a component poisoned by
    /// a panic (`poisoned` tells), so that a subscriber can let go of what
    /// it holds; the context is not published then.
    fn on_finish(&mut self) -> Result<(), HostError> {
        Ok(())
    }
}

/// An error of the host layer, or one a subscriber returns from a hook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    message: String,
}

impl HostError {
    pub fn new(message: impl Into<String>) -> Self {
        HostError {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostError {}

impl From<veryl_component::Error> for HostError {
    fn from(error: veryl_component::Error) -> Self {
        HostError::new(format!("{error:#}"))
    }
}

fn to_host_api(error: HostError) -> veryl_component::Error {
    veryl_component::anyhow!("{error}")
}

/// The hook entry the generated component calls: each function frames the
/// hook and calls the subscribers through `dispatch`, in the order given.
#[doc(hidden)]
pub mod entry {
    use super::*;

    pub fn on_build(
        ctx: &mut BuildCtx,
        dispatch: impl FnOnce(&mut BuildCtx) -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        build(ctx);
        dispatch(ctx).map_err(failure)
    }

    pub fn on_init(
        ctx: &mut SimCtx,
        dispatch: impl FnOnce() -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        init();
        FOUR_STATE.set(ctx.is_4state());
        event(ctx, || {}, dispatch)
    }

    /// `on_clock`: the tracked resets are released first (`on_clock` and
    /// `on_reset` are exclusive: a reset in force calls `on_reset`).
    pub fn on_clock(
        ctx: &mut SimCtx,
        dispatch: impl FnOnce() -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        event(ctx, release_resets, dispatch)
    }

    /// `on_reset`: the tracked resets that fired are asserted first.
    pub fn on_reset(
        ctx: &mut SimCtx,
        dispatch: impl FnOnce() -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        event(ctx, assert_fired_resets, dispatch)
    }

    fn event(
        ctx: &mut SimCtx,
        update: fn(),
        dispatch: impl FnOnce() -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        let Some(_guard) = enter(ctx) else {
            return Ok(());
        };
        update();
        let result = dispatch();
        leave();
        result.map_err(failure)
    }

    /// The test module's `start`: closes the registrations, then calls the
    /// subscribers' `on_start` within the hook's frame.
    pub fn start(
        ctx: &mut SimCtx,
        test: Option<&str>,
        dispatch: impl FnOnce(Option<&str>) -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        registry::close().map_err(to_host_api)?;
        let Some(_guard) = enter(ctx) else {
            return Ok(());
        };
        let result = dispatch(test);
        leave();
        result.map_err(failure)
    }

    /// A registration method's result, as the host API's.
    pub fn register(result: Result<(), HostError>) -> veryl_component::Result<()> {
        result.map_err(to_host_api)
    }

    pub fn on_finish(
        ctx: &mut SimCtx,
        dispatch: impl FnOnce() -> Result<(), HostError>,
    ) -> veryl_component::Result<()> {
        let guard = enter(ctx);
        let result = dispatch();
        if guard.is_some() {
            leave();
        }
        result.map_err(failure)
    }

    /// A subscriber's error: recorded as a failure, then returned to the
    /// host.
    fn failure(error: HostError) -> veryl_component::Error {
        FAILED.set(true);
        to_host_api(error)
    }
}

thread_local! {
    /// The context of the hook being run, or null between hooks.
    static CTX: Cell<*mut SimCtx> = const { Cell::new(std::ptr::null_mut()) };
    /// The seed Veryl gives the instance (`BuildCtx::seed`), taken at
    /// `create`.
    static SEED: Cell<u64> = const { Cell::new(0) };
    /// Set on entering a hook and cleared on a normal return: a hook that
    /// finds it set knows the previous one unwound.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
    /// Set once a hook has unwound: the component is in an unknown state,
    /// and every later hook returns at once.
    static POISONED: Cell<bool> = const { Cell::new(false) };
    /// Whether the simulation has failed: `fail` called, or a subscriber's
    /// error returned from a hook.
    static FAILED: Cell<bool> = const { Cell::new(false) };
    /// Whether the simulation is four-state, taken at `on_init`.
    static FOUR_STATE: Cell<bool> = const { Cell::new(false) };
    /// The runtime arguments, taken at `create`.
    static RUNTIME_ARGS: RefCell<Vec<RuntimeArg>> = const { RefCell::new(Vec::new()) };
    /// The resets tracked (`track_reset`), and whether each is in force.
    static RESETS: RefCell<Vec<(ResetPort, bool)>> = const { RefCell::new(Vec::new()) };
}

/// Takes what the layer keeps from `create`: the instance's seed, which
/// Veryl derives from the run's seed (`veryl test --seed`), the test and
/// the instance.
fn build(ctx: &mut BuildCtx) {
    SEED.with(|s| s.set(ctx.seed()));
    let args = ctx.runtime_args().to_vec();
    RUNTIME_ARGS.with(|a| *a.borrow_mut() = args);
}

/// The instance's seed, taken at `create`.
pub fn seed() -> u64 {
    SEED.with(Cell::get)
}

/// Starts a fresh simulation (`on_init`): forgets a panic of an earlier one
/// on this thread.
fn init() {
    IN_HOOK.set(false);
    POISONED.set(false);
    FAILED.set(false);
    RESETS.with(|r| r.borrow_mut().clear());
    registry::init();
}

/// Every runtime argument as given (`--runtime-arg`), in order, overridden
/// ones included.
pub fn runtime_args() -> Vec<RuntimeArg> {
    RUNTIME_ARGS.with(|a| a.borrow().clone())
}

/// The value of the runtime argument `name` that wins (the last one given),
/// or `None` if it was not given.
pub fn runtime_arg(name: &str) -> Option<ArgValue> {
    RUNTIME_ARGS.with(|a| find_arg(&a.borrow(), name).cloned())
}

/// The value of the runtime argument `name` read as `T`, or `None` if it
/// was not given. An error if it was given as a flag, or its value cannot
/// be read as `T`.
pub fn runtime_arg_value<T>(name: &str) -> Result<Option<T>, HostError>
where
    T: for<'a> TryFrom<&'a ArgValue, Error = ArgError>,
{
    match runtime_arg(name) {
        None => Ok(None),
        Some(value) => T::try_from(&value)
            .map(Some)
            .map_err(|e| HostError::new(format!("runtime argument `{name}`: {e}"))),
    }
}

/// The value of `name` among `args`: the last one given wins.
pub fn find_arg<'a>(args: &'a [RuntimeArg], name: &str) -> Option<&'a ArgValue> {
    args.iter()
        .rev()
        .find(|arg| arg.name == name)
        .map(|arg| &arg.value)
}

/// Tracks the reset `port`: from the next hook, `reset_asserted` tells
/// whether it is in force. Tracked resets are forgotten at `on_init`.
pub fn track_reset(port: ResetPort) {
    RESETS.with(|r| {
        let mut resets = r.borrow_mut();
        if !resets.iter().any(|(p, _)| *p == port) {
            resets.push((port, false));
        }
    });
}

/// Whether the tracked reset `port` is in force: asserted when `on_reset`
/// fired for it, released at `on_clock`. False for a port not tracked.
pub fn reset_asserted(port: ResetPort) -> bool {
    RESETS.with(|r| {
        r.borrow()
            .iter()
            .find(|(p, _)| *p == port)
            .is_some_and(|(_, asserted)| *asserted)
    })
}

fn release_resets() {
    RESETS.with(|r| {
        for (_, asserted) in r.borrow_mut().iter_mut() {
            *asserted = false;
        }
    });
}

fn assert_fired_resets() {
    RESETS.with(|r| {
        for (port, asserted) in r.borrow_mut().iter_mut() {
            if fired(*port) {
                *asserted = true;
            }
        }
    });
}

/// Enters a hook: checks for a panic in an earlier hook, marks this one as
/// running and publishes its context. `None` when the component is
/// poisoned and the hook has nothing to do.
#[doc(hidden)]
pub fn enter(ctx: &mut SimCtx) -> Option<HookGuard> {
    if IN_HOOK.replace(true) {
        // The previous hook unwound: a panic, reported by Veryl.
        POISONED.set(true);
    }
    if POISONED.get() {
        return None;
    }
    Some(HookGuard::enter(ctx))
}

/// Leaves a hook normally.
#[doc(hidden)]
pub fn leave() {
    IN_HOOK.set(false);
}

/// Whether a hook has unwound (a panic): the component is in an unknown
/// state, and only `on_finish` is still dispatched.
pub fn poisoned() -> bool {
    POISONED.get()
}

/// Publishes a hook's context for the duration of the hook.
#[doc(hidden)]
pub struct HookGuard {
    previous: *mut SimCtx,
}

impl HookGuard {
    fn enter(ctx: &mut SimCtx) -> HookGuard {
        let previous = CTX.with(|c| c.replace(ctx));
        HookGuard { previous }
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        CTX.with(|c| c.set(self.previous));
    }
}

/// Runs `f` with the context of the hook being run; `None` between hooks.
fn with_ctx<R>(f: impl FnOnce(&mut SimCtx) -> R) -> Option<R> {
    let ctx = CTX.with(Cell::get);
    if ctx.is_null() {
        None
    } else {
        // SAFETY: the pointer is set by `HookGuard` from the `&mut SimCtx` of
        // the hook being run and cleared when it returns. The runtime is
        // single-threaded and calls here never nest, so no other reference to
        // the context is live meanwhile.
        Some(f(unsafe { &mut *ctx }))
    }
}

fn outside(what: &str) -> ! {
    panic!("{what} is only possible within a simulator hook")
}

/// The number of 64-bit words holding `width` bits.
pub fn words_for(width: u32) -> usize {
    (width as usize).div_ceil(64).max(1)
}

/// The words of an input read through the host with its X/Z mask, X and Z
/// as 0: a four-state run, where a port's buffer holds Z as a 1 and only
/// the mask tells it.
fn read_two_state(ctx: &mut SimCtx, port: InputPort, out: &mut [u64]) {
    let value = ctx.read(port);
    let mask = value.mask_xz();
    for (i, (out, word)) in out.iter_mut().zip(value.as_slice()).enumerate() {
        *out = word & !mask.get(i).copied().unwrap_or(0);
    }
}

/// The value of an input up to 64 bits wide before the current edge, X and
/// Z as 0. Read through the port's buffer without a host call in a
/// two-state run.
pub fn read_u64(port: InputPort) -> u64 {
    with_ctx(|ctx| {
        if FOUR_STATE.get() {
            let mut word = [0];
            read_two_state(ctx, port, &mut word);
            word[0]
        } else {
            ctx.read_u64(port)
        }
    })
    .unwrap_or_else(|| outside("reading a signal"))
}

/// The words of an input (least significant first) before the current
/// edge into `out`, which holds at least `words_for(width)` words; X and Z
/// as 0. Read through the port's buffer without a host call in a two-state
/// run.
pub fn read_words(port: InputPort, out: &mut [u64]) {
    with_ctx(|ctx| {
        if FOUR_STATE.get() {
            read_two_state(ctx, port, out);
        } else {
            ctx.read_words(port, out);
        }
    })
    .unwrap_or_else(|| outside("reading a signal"))
}

/// Drives an output up to 64 bits wide; the value reaches the design at the
/// current edge. Written into the port's buffer without building a `Value`;
/// X and Z are not driven.
pub fn write_u64(port: OutputPort, word: u64) {
    with_ctx(|ctx| ctx.write_u64(port, word)).unwrap_or_else(|| outside("writing a signal"))
}

/// Drives an output from its words (least significant first, at least
/// `words_for(width)` of them); the value reaches the design at the current
/// edge. X and Z are not driven.
pub fn write_words(port: OutputPort, words: &[u64]) {
    with_ctx(|ctx| ctx.write_words(port, words)).unwrap_or_else(|| outside("writing a signal"))
}

/// The current simulation time; 0 outside a hook.
pub fn time() -> u64 {
    with_ctx(|ctx| ctx.time()).unwrap_or(0)
}

/// Writes a line to the test's output.
pub fn log(line: &str) {
    if with_ctx(|ctx| ctx.log(line)).is_none() {
        println!("{line}");
    }
}

/// Requests the end of the simulation at the end of the current cycle.
pub fn finish() {
    with_ctx(|ctx| ctx.finish());
}

/// Fails the test.
pub fn fail(message: &str) {
    FAILED.set(true);
    with_ctx(|ctx| ctx.fail(message));
}

/// Whether the simulation has failed: `fail` called, or a subscriber's
/// error returned from a hook. A finish it caused is expected.
pub fn failed() -> bool {
    FAILED.get()
}

/// Whether `port` is the clock or reset that fired the current hook.
pub fn fired(port: impl Into<InputPort>) -> bool {
    let port = port.into();
    with_ctx(|ctx| ctx.clock() == port).unwrap_or(false)
}
