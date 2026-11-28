//! Verald: verification for Veryl in Rust. This crate is the entry point:
//! it re-exports Verald's parts, chosen by features (`tb`, `random` and
//! `assertion` by default), with a prelude. See `docs/design.md` in the
//! repository for the design.
//!
//! - [`host`], the layer over Veryl's host API, shared by the parts;
//! - [`tb`], the UVM-style testbench (`tb::runtime`, `tb::core`);
//! - [`random`], constrained random;
//! - [`assertion`], assertions.
//!
//! A testbench crate (a `cdylib`) registers its tests and exports one
//! component, with the testbench, the assertions, or both:
//!
//! ```ignore
//! use verald::prelude::*;
//!
//! register_test!(RegTest);
//! verald::export!("tb", testbench = Methodology, assertions = [BusAssertions]);
//! ```

pub use verald_host as host;

#[cfg(feature = "random")]
pub use verald_random as random;

#[cfg(feature = "assertion")]
pub use verald_assertion as assertion;

/// The UVM-style testbench.
#[cfg(feature = "tb")]
pub mod tb {
    pub use verald_tb_core as core;
    pub use verald_tb_runtime as runtime;
}

/// The names a testbench usually needs.
pub mod prelude {
    #[cfg(feature = "assertion")]
    pub use verald_assertion::{assertions, check, countones, onehot, onehot0, seq};
    #[cfg(not(feature = "tb"))]
    pub use verald_host::modport;
    #[cfg(feature = "random")]
    pub use verald_random::{
        RandEnum, constraint_fn, constraints, randcase, randomize, randomize_with, std_randomize,
    };
    #[cfg(feature = "tb")]
    pub use verald_tb_core::prelude::*;
    #[cfg(feature = "tb")]
    pub use verald_tb_runtime::prelude::*;
}

/// Exports the component as `$comp::<name>`. Invoke it once in the crate
/// (a `cdylib`), which must depend on `veryl-component` directly.
///
/// - `export!("tb")`: the testbench, running the tests registered with
///   `register_test!` (the methodology, as `testbench = Methodology`);
/// - `export!("tb", testbench = T)`: the testbench over `T`;
/// - `export!("tb", testbench = T, assertions = [A, B])`: the testbench and
///   the assertion blocks `A` and `B`, evaluated before it at each edge;
/// - `export!("checks", assertions = [A, B])`: the assertions alone, started
///   by the test module with `start()`, with no test's name.
///
/// With assertions, the test module binds blocks with
/// `bind_to_instance(path, block)` before `start`.
#[macro_export]
macro_rules! export {
    ($name:literal) => {
        $crate::export!($name, testbench = $crate::tb::core::Methodology);
    };
    ($name:literal, testbench = $tb:ty $(,)?) => {
        $crate::tb::runtime::export!($name, $tb);
    };
    ($name:literal, testbench = $tb:ty, assertions = [$($a:ty),+ $(,)?] $(,)?) => {
        $crate::tb::runtime::component!(
            VeraldComponent,
            $tb,
            before { assertions: $crate::assertion::Assertions<$crate::assertion::blocks![$($a),+]> }
            methods {
                /// Binds the assertion block `block` at the instance `path`
                /// (wildcards allowed), every member by its name.
                fn bind_to_instance(
                    &mut self,
                    _ctx: &mut ::veryl_component::SimCtx,
                    path: ::std::string::String,
                    block: ::std::string::String,
                ) -> ::veryl_component::Result<()> {
                    $crate::host::entry::register(self.assertions.bind_to_instance(path, block))
                }

                /// Binds the assertion block `block` at every instance of
                /// `module` (not supported yet).
                fn bind_to_module(
                    &mut self,
                    _ctx: &mut ::veryl_component::SimCtx,
                    module: ::std::string::String,
                    block: ::std::string::String,
                ) -> ::veryl_component::Result<()> {
                    $crate::host::entry::register(self.assertions.bind_to_module(module, block))
                }
            }
        );
        ::veryl_component::veryl_component_export!($name => VeraldComponent);
    };
    ($name:literal, assertions = [$($a:ty),+ $(,)?] $(,)?) => {
        $crate::host::component! {
            /// Verald's assertions: the test module registers clocks, resets
            /// and modports, binds assertion blocks, then starts them.
            struct VeraldComponent { assertions: $crate::assertion::Assertions<$crate::assertion::blocks![$($a),+]> }

            /// Starts the assertions: binds the blocks asked for.
            fn start(
                &mut self,
                ctx: &mut ::veryl_component::SimCtx,
            ) -> ::veryl_component::Result<()> {
                $crate::host::entry::start(ctx, ::core::option::Option::None, |test| {
                    self.__verald_on_start(test)
                })
            }

            /// Binds the assertion block `block` at the instance `path`
            /// (wildcards allowed), every member by its name.
            fn bind_to_instance(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                path: ::std::string::String,
                block: ::std::string::String,
            ) -> ::veryl_component::Result<()> {
                $crate::host::entry::register(self.assertions.bind_to_instance(path, block))
            }

            /// Binds the assertion block `block` at every instance of
            /// `module` (not supported yet).
            fn bind_to_module(
                &mut self,
                _ctx: &mut ::veryl_component::SimCtx,
                module: ::std::string::String,
                block: ::std::string::String,
            ) -> ::veryl_component::Result<()> {
                $crate::host::entry::register(self.assertions.bind_to_module(module, block))
            }
        }
        ::veryl_component::veryl_component_export!($name => VeraldComponent);
    };
}
