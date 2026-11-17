//! The simulator boundary: what the runtime asks of the simulator.
//!
//! Signals are read and written only within a hook, with clocking-block
//! semantics: a read returns the value before the edge, a write reaches the
//! design at the edge. Only Veryl's simulator is served for now, through the
//! `SimCtx` of the hook being run, published here for its duration.

use std::cell::Cell;

use veryl_component::{InputPort, OutputPort, SimCtx, Value};

thread_local! {
    /// The context of the hook being run, or null between hooks.
    static CTX: Cell<*mut SimCtx> = const { Cell::new(std::ptr::null_mut()) };
}

/// Publishes a hook's context for the duration of the hook.
pub(crate) struct HookGuard {
    previous: *mut SimCtx,
}

impl HookGuard {
    pub(crate) fn enter(ctx: &mut SimCtx) -> HookGuard {
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

/// The value of an input before the current edge.
pub fn read(port: impl Into<InputPort>) -> Value {
    let port = port.into();
    with_ctx(|ctx| ctx.read(port)).unwrap_or_else(|| outside("reading a signal"))
}

/// Drives an output; the value reaches the design at the current edge.
pub fn write(port: OutputPort, value: impl Into<Value>) {
    let value = value.into();
    with_ctx(|ctx| ctx.write(port, value)).unwrap_or_else(|| outside("writing a signal"))
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
pub(crate) fn finish() {
    with_ctx(|ctx| ctx.finish());
}

/// Fails the test.
pub(crate) fn fail(message: &str) {
    with_ctx(|ctx| ctx.fail(message));
}

/// Whether `port` is the clock or reset that fired the current hook.
pub(crate) fn fired(port: impl Into<InputPort>) -> bool {
    let port = port.into();
    with_ctx(|ctx| ctx.clock() == port).unwrap_or(false)
}
