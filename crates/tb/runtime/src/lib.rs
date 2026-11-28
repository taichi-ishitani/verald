//! The runtime of verald: a single-threaded executor driven by Veryl's
//! simulator, waits for clocks and resets, the parts tasks share, and the
//! simulator boundary.
//!
//! A testbench crate exports one component with [`export!`]; the Veryl test
//! module registers clocks, resets and interfaces with it and calls
//! `start`. Everything then runs in the component's hooks: at each clock
//! edge the tasks waiting for it are woken and run until they wait again.

#![allow(async_fn_in_trait)]

pub mod args;
pub mod clocking;
mod concurrency;
mod error;
mod executor;
pub mod testbench;
/// The host layer, re-exported: see the `verald-host` crate.
pub use verald_host as host;
mod signals;
mod state;
mod sync;

pub use args::{
    ArgError, ArgValue, RuntimeArg, runtime_arg, runtime_arg_exists, runtime_arg_value,
    runtime_args,
};
pub use clocking::{Clocking, ClockingPaths, ClockingTimeout, Clocks};
pub use concurrency::{Either, ForkScope, first, fork_scope, join, try_join, yield_now};
pub use error::TestError;
pub use host::{HostError, SignalValue, Signals};
pub use signals::{Clock, Edges, Reset, ResetWait, run_with_reset};
pub use state::RunEnd;
pub use sync::{Event, Fifo, Permit, Semaphore};
pub use testbench::{Start, Testbench};
pub use verald_tb_macros::{
    changed, changed_within, clocking, edge, edge_within, negedge, negedge_within, posedge,
    posedge_within, tick_until, tick_until_within, tick_while, tick_while_within,
};
pub use veryl_component::modport;

/// The names a testbench usually needs.
pub mod prelude {
    pub use crate::{
        ArgValue, Clock, Clocking, ClockingPaths, ClockingTimeout, Clocks, Either, Event, Fifo,
        ForkScope, Permit, Reset, Semaphore, SignalValue, Signals, Start, TestError, Testbench,
        changed, changed_within, clocking, edge, edge_within, first, fork_scope, join, modport,
        negedge, negedge_within, posedge, posedge_within, run_with_reset, runtime_arg,
        runtime_arg_exists, runtime_arg_value, runtime_args, tick, tick_until, tick_until_within,
        tick_while, tick_while_within, try_join,
    };
}

pub use num_bigint::BigUint;

#[doc(hidden)]
pub mod __private {
    pub use veryl_component;
}

/// Defines the testbench component type `$ty` over the [`Testbench`] `$tb`,
/// without exporting it: [`export!`] does both. Useful on its own to drive
/// the component from `veryl_component::testing` in unit tests.
///
/// The component is the host layer's (`host::component!`) with the runtime
/// as its subscriber, and the testbench's own methods: `start(test)`,
/// `set_master_clock`, `option` and `set`.
///
/// The crate must depend on `veryl-component` directly: its derives name
/// `::veryl_component`.
///
/// `before { field: Subscriber, .. } methods { .. }` adds subscribers
/// dispatched before the runtime (the assertions) and their methods.
#[macro_export]
macro_rules! component {
    ($vis:vis $ty:ident, $tb:ty) => {
        $crate::component!($vis $ty, $tb, before {} methods {});
    };
    (
        $vis:vis $ty:ident, $tb:ty,
        before { $($field:ident : $sub:ty),* $(,)? }
        methods { $($methods:tt)* }
    ) => {
        $crate::host::component! {
            /// The Verald testbench: the test module registers clocks,
            /// resets and interfaces, gives options and settings, then starts
            /// a test.
            $vis struct $ty { $($field: $sub,)* runtime: $crate::testbench::Runtime<$tb> }

            $($methods)*

            /// Starts the test `test`, as UVM's `run_test`.
            fn start(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                test: ::std::string::String,
            ) -> ::veryl_component::Result<()> {
                $crate::host::entry::start(ctx, ::core::option::Option::Some(&test), |test| {
                    self.__verald_on_start(test)
                })
            }

            /// Makes the clock registered as `name` the master clock, which
            /// the timeout counts (the first registered by default).
            fn set_master_clock(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
            ) -> ::veryl_component::Result<()> {
                $crate::testbench::set_master_clock(ctx, name)
            }

            /// Gives an option of the framework, checked at once.
            fn option(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                key: ::std::string::String,
                value: ::veryl_component::Value,
            ) -> ::veryl_component::Result<()> {
                $crate::testbench::option::<$tb>(ctx, key, value)
            }

            /// Gives a setting of the testbench, read when the test starts.
            fn set(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                key: ::std::string::String,
                value: ::veryl_component::Value,
            ) -> ::veryl_component::Result<()> {
                $crate::testbench::set(ctx, key, value)
            }
        }
    };
}

/// Exports the testbench component as `$comp::<name>`, over the
/// [`Testbench`] `$tb`. Invoke it once in the testbench crate (a `cdylib`).
///
/// ```ignore
/// verald_tb_runtime::export!("tb", MyTestbench);
/// ```
#[macro_export]
macro_rules! export {
    ($name:literal, $tb:ty) => {
        $crate::component!(VeraldComponent, $tb);
        ::veryl_component::veryl_component_export!($name => VeraldComponent);
    };
}
