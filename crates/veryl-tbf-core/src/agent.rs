//! What `#[component(agent(..))]` builds agents from: the configuration's
//! mode, and the ports of the parts, which `#[component(..)]` on their
//! structs implements from field markers.

use std::future::Future;

use crate::context::InstanceContext;
use crate::sequence::SequenceCore;
use crate::sequencer::{SeqItemPort, Sequencer};
use crate::tlm::{AnalysisFifo, AnalysisPort};

/// An agent's configuration: whether it drives (active) or only watches
/// (passive), and whether a passive agent keeps a sequencer (tue's
/// `ENABLE_PASSIVE_SEQUENCER`), for a reactive sequence to answer on.
pub trait AgentConfig {
    fn active(&self) -> bool;
    fn passive_sequencer(&self) -> bool {
        false
    }
}

/// An agent's context, whose configuration gives its mode.
#[doc(hidden)]
pub trait AgentContext: Clone {
    fn active(&self) -> bool;
    fn passive_sequencer(&self) -> bool;
}

impl<C: AgentConfig, S> AgentContext for InstanceContext<C, S> {
    fn active(&self) -> bool {
        self.config().active()
    }
    fn passive_sequencer(&self) -> bool {
        self.config().passive_sequencer()
    }
}

/// A monitor's analysis port, from its field marked `#[analysis_port]`.
pub trait MonitorPort {
    type Item: 'static;
    fn analysis_port(&self) -> AnalysisPort<Self::Item>;
}

/// A reactive monitor's request port, from its field marked
/// `#[request_port]`: the requests it sees, for the sequencer to answer.
pub trait RequestPort {
    type Request: 'static;
    fn request_port(&self) -> AnalysisPort<Self::Request>;
}

/// A driver's sequence item port, from its field marked `#[seq_item_port]`.
pub trait DriverPort {
    type Item: 'static;
    type Response: 'static;
    fn seq_item_port(&mut self) -> &mut SeqItemPort<Self::Item, Self::Response>;
}

/// A sequencer that takes requests (`#[component(sequencer(item = ..,
/// request = ..))]`), for reactive sequences.
pub trait ReactiveSequencer: Sequencer {
    type Request: Clone + 'static;
    #[doc(hidden)]
    fn __requests(&self) -> &AnalysisFifo<Self::Request>;
}

/// What a sequence on a reactive sequencer can do: take the next request.
pub trait ReactiveSequence: SequenceCore
where
    Self::Sequencer: ReactiveSequencer,
{
    /// The next request the monitor saw, waiting for one.
    fn get_request(&self) -> impl Future<Output = <Self::Sequencer as ReactiveSequencer>::Request> {
        let fifo = self.sequencer().__requests().clone();
        async move { fifo.get().await }
    }
}

impl<Q> ReactiveSequence for Q
where
    Q: SequenceCore,
    Q::Sequencer: ReactiveSequencer,
{
}
