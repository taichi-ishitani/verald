//! The UVM-like methodology of veryl-tbf: components and their tree, the
//! phases, tests and their specs, and the runner, on the runtime.

#![allow(async_fn_in_trait)]

mod agent;
mod component;
mod context;
mod item;
mod report;
mod seeds;
mod sequence;
mod sequencer;
mod test;
mod tlm;

pub use agent::{
    AgentConfig, DriverPort, MonitorPort, ReactiveSequence, ReactiveSequencer, RequestPort,
};
pub use component::{
    CheckContext, Component, Instance, Kind, OptInstance, ReportContext, RunContext, Scope,
};
pub use context::{HasContext, InstanceContext, Knob};
pub use item::ItemHandle;
pub use report::Severity;
pub use sequence::{HasBase, Sequence, SequenceCore};
pub use sequencer::{ArbRequest, Arbiter, FifoArbiter, SeqItemPort, Sequencer, SequencerHandle};
pub use test::{BundledSpecs, FromSetting, Methodology, Scenario, SetupContext, Spec, Test};
pub use tlm::{AnalysisExport, AnalysisFifo, AnalysisPort, ItemWaiter, Subscriber};
pub use veryl_tbf_macros::{component, register_test, sequence};
pub use veryl_tbf_random::SeedSource;

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
        AgentConfig, AnalysisExport, AnalysisFifo, AnalysisPort, BundledSpecs, CheckContext,
        Component, DriverPort, HasBase, HasContext, Instance, InstanceContext, ItemHandle,
        ItemWaiter, Kind, Knob, MonitorPort, OptInstance, ReactiveSequence, ReactiveSequencer,
        ReportContext, RequestPort, RunContext, Scenario, Scope, SeqItemPort, Sequence,
        SequenceCore, Sequencer, SequencerHandle, SetupContext, Spec, Subscriber, Test,
        bundled_specs, component, register_test, sequence,
    };
}

#[doc(hidden)]
pub mod __private {
    pub use crate::agent::AgentContext;
    pub use crate::component::TaskFuture;
    pub use crate::context::{ContextParts, DeclaredKind};
    pub use crate::sequence::SequenceFields;
    pub use crate::sequencer::SequencerCore;
    pub use crate::test::{SpecTarget, TestEntry, start_test};
    pub use inventory;
    pub use veryl_tbf_runtime::TestError;
}
