//! veryl-tbf: a UVM-style verification framework in Rust for Veryl.
//!
//! This crate is the entry point: it re-exports the framework's crates. See
//! `docs/design.md` in the repository for the design.

pub use veryl_tbf_runtime as runtime;

/// The names a testbench usually needs.
pub mod prelude {
    pub use veryl_tbf_runtime::prelude::*;
}
