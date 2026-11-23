//! Verald: verification for Veryl in Rust. This crate is the entry point:
//! it re-exports Verald's parts, chosen by features (`tb` and `random` by
//! default), with a prelude. See `docs/design.md` in the repository for the
//! design.
//!
//! - [`host`]: the layer over Veryl's host API, shared by the parts;
//! - [`tb`]: the UVM-style testbench (`tb::runtime`, `tb::core`);
//! - [`random`]: constrained random.
//!
//! A testbench crate (a `cdylib`) registers its tests and exports the
//! testbench component once:
//!
//! ```ignore
//! use verald::prelude::*;
//!
//! register_test!(RegTest);
//! verald::export!("tb");
//! ```

pub use verald_host as host;

#[cfg(feature = "random")]
pub use verald_random as random;

/// The UVM-style testbench.
#[cfg(feature = "tb")]
pub mod tb {
    pub use verald_tb_core as core;
    pub use verald_tb_runtime as runtime;
}

/// The names a testbench usually needs.
pub mod prelude {
    #[cfg(feature = "random")]
    pub use verald_random::{
        RandEnum, constraint_fn, constraints, randcase, randomize, randomize_with, std_randomize,
    };
    #[cfg(feature = "tb")]
    pub use verald_tb_core::prelude::*;
    #[cfg(feature = "tb")]
    pub use verald_tb_runtime::prelude::*;
}

/// Exports the testbench component as `$comp::<name>`, running the tests
/// registered with `register_test!`. Invoke it once in the testbench crate,
/// which must depend on `veryl-component` directly.
#[cfg(feature = "tb")]
#[macro_export]
macro_rules! export {
    ($name:literal) => {
        $crate::tb::runtime::export!($name, $crate::tb::core::Methodology);
    };
}
