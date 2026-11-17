//! veryl-tbf: a UVM-style verification framework in Rust for Veryl.
//!
//! This crate is the entry point: it re-exports the framework's crates. See
//! `docs/design.md` in the repository for the design.
//!
//! A testbench crate (a `cdylib`) registers its tests and exports the
//! testbench component once:
//!
//! ```ignore
//! use veryl_tbf::prelude::*;
//!
//! register_test!(RegTest);
//! veryl_tbf::export!("tb");
//! ```

pub use veryl_tbf_core as core;
pub use veryl_tbf_random as random;
pub use veryl_tbf_runtime as runtime;

/// The names a testbench usually needs.
pub mod prelude {
    pub use veryl_tbf_core::prelude::*;
    pub use veryl_tbf_random::{
        RandEnum, constraint_fn, constraints, randcase, randomize, randomize_with, std_randomize,
    };
    pub use veryl_tbf_runtime::prelude::*;
}

/// Exports the testbench component as `$comp::<name>`, running the tests
/// registered with `register_test!`. Invoke it once in the testbench crate,
/// which must depend on `veryl-component` directly.
#[macro_export]
macro_rules! export {
    ($name:literal) => {
        $crate::runtime::export!($name, $crate::core::Methodology);
    };
}
