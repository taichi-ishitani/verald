//! The UVM-like methodology of veryl-tbf: components and their tree, the
//! phases, tests and their specs, and the runner, on the runtime.

#![allow(async_fn_in_trait)]

mod component;
mod report;
mod test;

pub use component::{
    CheckContext, Component, Instance, Kind, OptInstance, ReportContext, RunContext, Scope,
};
pub use report::Severity;
pub use test::{BundledSpecs, FromSetting, Methodology, SetupContext, Spec, Test};
pub use veryl_tbf_macros::register_test;

/// Bundles specs, applied in the order given: `bundled_specs![SlowWrites,
/// InjectErrors]` is `BundledSpecs::new().with(SlowWrites).with(InjectErrors)`.
#[macro_export]
macro_rules! bundled_specs {
    ($($spec:expr),* $(,)?) => {
        $crate::BundledSpecs::new()$(.with($spec))*
    };
}

/// The names a testbench usually needs.
pub mod prelude {
    pub use crate::{
        BundledSpecs, CheckContext, Component, Instance, Kind, OptInstance, ReportContext,
        RunContext, Scope, SetupContext, Spec, Test, bundled_specs, register_test,
    };
}

#[doc(hidden)]
pub mod __private {
    pub use crate::test::{TestEntry, start_test};
    pub use inventory;
}
