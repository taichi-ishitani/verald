//! The runtime of veryl-tbf: a single-threaded executor driven by Veryl's
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
pub mod host;
mod signals;
pub mod sim;
mod state;
mod sync;

pub use args::{
    ArgError, ArgValue, RuntimeArg, runtime_arg, runtime_arg_exists, runtime_arg_value,
    runtime_args,
};
pub use clocking::{Clocking, ClockingPaths, ClockingTimeout, SignalValue, Signals};
pub use concurrency::{Either, ForkScope, first, fork_scope, join, try_join, yield_now};
pub use error::TestError;
pub use host::{Start, Testbench};
pub use signals::{Clock, Edges, Reset, ResetWait, run_with_reset};
pub use state::RunEnd;
pub use sync::{Event, Fifo};
pub use veryl_tbf_macros::{
    changed, changed_within, clocking, edge, edge_within, modport, negedge, negedge_within,
    posedge, posedge_within, tick_until, tick_until_within, tick_while, tick_while_within,
};

/// The names a testbench usually needs.
pub mod prelude {
    pub use crate::{
        ArgValue, Clock, Clocking, ClockingPaths, ClockingTimeout, Either, Event, Fifo, ForkScope,
        Reset, SignalValue, Signals, Start, TestError, Testbench, changed, changed_within,
        clocking, edge, edge_within, first, fork_scope, join, modport, negedge, negedge_within,
        posedge, posedge_within, run_with_reset, runtime_arg, runtime_arg_exists,
        runtime_arg_value, runtime_args, tick, tick_until, tick_until_within, tick_while,
        tick_while_within, try_join,
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
/// The crate must depend on `veryl-component` directly: its derives name
/// `::veryl_component`.
#[macro_export]
macro_rules! component {
    ($vis:vis $ty:ident, $tb:ty) => {
        /// The veryl-tbf testbench: the test module registers clocks,
        /// resets and interfaces, gives options and settings, then starts
        /// a test.
        #[derive(::veryl_component::Component)]
        #[component(kind = clocked)]
        $vis struct $ty {
            host: $crate::host::Host,
        }

        #[::veryl_component::component_impl]
        impl $ty {
            fn on_build(
                &mut self,
                ctx: &mut ::veryl_component::BuildCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::host::on_build(ctx)
            }

            fn on_init(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::host::on_init::<$tb>(ctx)
            }

            fn on_clock(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::host::on_clock(ctx)
            }

            fn on_reset(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::host::on_reset(ctx)
            }

            fn on_finish(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::host::on_finish(ctx)
            }

            /// Registers a clock under `name`.
            fn set_clock(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                clk: ::veryl_component::ClockPort,
            ) -> ::veryl_component::Result<()> {
                $crate::host::set_clock(ctx, name, clk)
            }

            /// Registers a reset under `name`.
            fn set_reset(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                rst: ::veryl_component::ResetPort,
            ) -> ::veryl_component::Result<()> {
                $crate::host::set_reset(ctx, name, rst)
            }

            /// Registers an interface (a modport) under `name`.
            fn set_interface(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                bus: ::veryl_component::ModportRef,
            ) -> ::veryl_component::Result<()> {
                $crate::host::set_interface(ctx, name, bus)
            }

            /// Registers a signal the testbench reads under `name`.
            fn set_input(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                sig: ::veryl_component::InputPort,
            ) -> ::veryl_component::Result<()> {
                $crate::host::set_input(ctx, name, sig)
            }

            /// Registers a signal the testbench drives under `name`.
            fn set_output(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                name: ::std::string::String,
                sig: ::veryl_component::OutputPort,
            ) -> ::veryl_component::Result<()> {
                $crate::host::set_output(ctx, name, sig)
            }

            /// Gives an option of the framework, checked at once.
            fn option(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                key: ::std::string::String,
                value: ::veryl_component::Value,
            ) -> ::veryl_component::Result<()> {
                $crate::host::option::<$tb>(ctx, key, value)
            }

            /// Gives a setting of the testbench, read when the test starts.
            fn set(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                key: ::std::string::String,
                value: ::veryl_component::Value,
            ) -> ::veryl_component::Result<()> {
                $crate::host::set(ctx, key, value)
            }

            /// Starts the test `test`, as UVM's `run_test`.
            fn start(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
                test: ::std::string::String,
            ) -> ::veryl_component::Result<()> {
                $crate::host::start::<$tb>(ctx, test)
            }
        }
    };
}

/// Exports the testbench component as `$comp::<name>`, over the
/// [`Testbench`] `$tb`. Invoke it once in the testbench crate (a `cdylib`).
///
/// ```ignore
/// veryl_tbf_runtime::export!("tb", MyTestbench);
/// ```
#[macro_export]
macro_rules! export {
    ($name:literal, $tb:ty) => {
        $crate::component!(TbfComponent, $tb);
        ::veryl_component::veryl_component_export!($name => TbfComponent);
    };
}
