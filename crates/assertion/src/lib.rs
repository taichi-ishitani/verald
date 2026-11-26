//! Assertions for Verald: SVA-like properties over a design's signals,
//! evaluated by a component at each edge of their clock. See
//! `docs/assertion-design.md`.
//!
//! An assertion block is a struct and an impl marked `#[assertions]`: the
//! struct's `#[clock]`, `#[reset]`, `#[input]` and `#[modport]` fields are
//! the signals it reads, its other fields auxiliary state; the impl holds
//! the properties (`#[assert]`) and the hooks (`#[on_clock]`,
//! `#[on_reset]`). A property's sequences over cycles are written in
//! `seq!`, their elements chained by `->` (booleans, `delay(1..=3)`,
//! `let name: T = x`, `wait(b)`, `happen(n, b)`, `repeat(n, s)`,
//! `throughout(a, s)`, `until(a, b)`); `ended(s)` is true where a match of
//! `s` ends; a body may end with `eventually(x)` or `never(s)`; `past`,
//! `rose`, `fell`, `stable` and `changed` read earlier edges. At the end of
//! the test, attempts still running are reported incomplete (`eventually`'s
//! as failures).
//!
//! ```ignore
//! #[assertions]
//! #[derive(Default)]
//! struct BusAssertions {
//!     #[clock]   clk: Clock,
//!     #[reset]   rst: Reset,
//!     #[modport] bus: BusWatch,
//!     reads: u32,
//! }
//!
//! #[assertions]
//! impl BusAssertions {
//!     // valid && ready && !write |=> rvalid
//!     #[assert]
//!     fn rvalid_follows_a_read(&self) {
//!         if bus.valid && bus.ready && !bus.write {
//!             seq!(delay(1) -> bus.rvalid);
//!         }
//!     }
//!
//!     // (valid && !ready, a = addr) |=> valid && addr == a
//!     #[assert]
//!     fn a_stalled_request_is_held(&self) {
//!         if bus.valid && !bus.ready {
//!             let a: u8 = bus.addr;
//!             seq!(delay(1) -> bus.valid && bus.addr == a);
//!         }
//!     }
//!
//!     #[on_clock]
//!     fn count(&mut self) {
//!         if bus.rvalid {
//!             check!(past(bus.valid && bus.ready), "rvalid with no request");
//!             self.reads += 1;
//!         }
//!     }
//! }
//! ```
//!
//! A component holding the blocks is exported with `verald::export!`
//! (`assertions = [BusAssertions]`), and the test module binds them:
//! `tb.bind_to_instance("", "BusAssertions")`.

mod engine;
mod functions;
#[doc(hidden)]
pub mod temporal;

pub use engine::{Assertions, BlockList, Instance};
pub use functions::{Bits, countones, onehot, onehot0};
pub use temporal::History;
pub use verald_assertion_macros::assertions;
pub use verald_macros::modport;

use verald_host::{self as host, ClockPort, HostError, ResetPort, Signals};

/// The signals of an assertion block, as `#[assertions]` on its struct
/// generates them: bound by name at an instance path, read at each edge.
pub trait AssertionSignals: Sized {
    /// The block's name, as `bind_to_instance` takes it.
    const NAME: &'static str;
    /// Binds every member at `path` (`<path>.<member>`, or the member's
    /// name at the root).
    fn bind<S: Signals + ?Sized>(signals: &S, path: &str) -> Result<Self, HostError>;
    /// The clock the block is evaluated at.
    fn clock(&self) -> ClockPort;
    /// The reset that disables it, if any.
    fn reset(&self) -> Option<ResetPort>;
    /// Reads every member's value before the current edge.
    fn sample(&mut self);
}

/// An assertion block, as `#[assertions]` on its impl generates it: the
/// properties and hooks over its signals. The block's own fields are its
/// auxiliary state, re-initialized from `Default` at a reset.
pub trait AssertionBlock: Default + 'static {
    type Signals: AssertionSignals;
    /// The properties' attempts in flight, dropped at a reset.
    type State: Default;
    /// How many edges back `past` reads, at most.
    const PAST: usize;
    /// Steps every property by one edge, reporting the ones that fail.
    fn check(&self, state: &mut Self::State, signals: &History<Self::Signals>, report: &Report<'_>);
    /// Runs the `#[on_clock]` hooks, after the properties.
    fn on_clock(&mut self, signals: &History<Self::Signals>, report: &Report<'_>);
    /// Runs the `#[on_reset]` hooks, after the state is re-initialized.
    fn on_reset(&mut self, signals: &History<Self::Signals>, report: &Report<'_>);
    /// At the end of the test: reports the attempts still running, as
    /// incomplete, or as failures for a strong property (`eventually`).
    fn finish(&self, state: &mut Self::State, report: &Report<'_>);
}

/// Where in the source a failure was written: the crate's name and the
/// file's path in it (`regblock_assertions/src/lib.rs`), and the line.
/// `#[assertions]` makes one at compile time for each expectation and each
/// `check!`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub file: &'static str,
    pub line: u32,
}

/// A property's expectation that did not hold, as written, and where.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Failure {
    pub expr: &'static str,
    pub location: Location,
}

/// Where a bound block reports its failures: the bind's name and instance
/// path. A failure fails the test, which ends at the end of the cycle.
pub struct Report<'a> {
    label: &'a str,
}

impl<'a> Report<'a> {
    #[doc(hidden)]
    pub fn new(label: &'a str) -> Self {
        Report { label }
    }

    /// The bind's name and instance path (`BusAssertions@dut`).
    pub fn label(&self) -> &str {
        self.label
    }

    /// A property failed: `failure` is the expectation that did not hold,
    /// in an attempt started at `start`, into which `merged` other
    /// attempts merged.
    #[doc(hidden)]
    pub fn property_failed(&self, property: &str, failure: &Failure, start: u64, merged: u32) {
        let Location { file, line } = failure.location;
        let now = host::time();
        let started = match (start == now, merged) {
            (true, _) => String::new(),
            (false, 0) => format!(" (started at {start})"),
            (false, 1) => format!(" (started at {start}, and 1 more attempt)"),
            (false, n) => format!(" (started at {start}, and {n} more attempts)"),
        };
        host::fail(&format!(
            "assertion `{}.{property}` failed at {now}{started}: `{}` ({file}:{line})",
            self.label, failure.expr
        ));
    }

    /// At the end of the test, `count` attempts of a property were still
    /// running, the earliest started at `earliest`: not a failure (the
    /// test ended before they could), only reported.
    #[doc(hidden)]
    pub fn incomplete(&self, property: &str, count: u64, earliest: u64) {
        let attempts = if count == 1 { "attempt" } else { "attempts" };
        host::log(&format!(
            "assertion `{}.{property}`: {count} {attempts} incomplete at the end, \
             the earliest started at {earliest}",
            self.label
        ));
    }

    /// A `check!` in a hook failed.
    #[doc(hidden)]
    pub fn check_failed(&self, cond: &str, message: &str, location: Location) {
        let message = if message.is_empty() {
            format!("`{cond}`")
        } else {
            format!("{message} (`{cond}`)")
        };
        let Location { file, line } = location;
        host::fail(&format!(
            "check in `{}` failed at {}: {message} ({file}:{line})",
            self.label,
            host::time()
        ));
    }
}

/// The list of block types a component holds, as [`BlockList`] takes it:
/// `blocks![BusAssertions, FifoAssertions]` is
/// `(BusAssertions, (FifoAssertions, ()))`. `verald::export!` writes it
/// from `assertions = [..]`.
#[macro_export]
macro_rules! blocks {
    () => { () };
    ($head:ty $(, $rest:ty)* $(,)?) => { ($head, $crate::blocks![$($rest),*]) };
}

/// An immediate check in an `#[on_clock]` or `#[on_reset]` hook, as
/// `$assert` in an `always_ff`: `check!(cond)` or
/// `check!(cond, "format", args..)`. A failure is reported as a property's
/// is, and fails the test. `#[assertions]` rewrites it within hooks; used
/// elsewhere, it is an error.
#[macro_export]
macro_rules! check {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`check!` is only allowed in an `#[on_clock]` or `#[on_reset]` hook of an `#[assertions]` impl"
        )
    };
}

/// A sequence over cycles, in a property: its elements chained by `->`
/// (`seq!(req -> delay(1..=4) -> ack)`). `#[assertions]` compiles it with
/// the property; used elsewhere, it is an error.
#[macro_export]
macro_rules! seq {
    ($($t:tt)*) => {
        ::core::compile_error!("`seq!` is only allowed in a property of an `#[assertions]` impl")
    };
}

/// Items the generated code names, independent of the user's imports.
#[doc(hidden)]
pub mod __private {
    pub use crate::functions::lsb;
    pub use crate::temporal::{Antecedent, Consequent, Failed, Out, States};
    pub use verald_assertion_macros::__assertions_impl;
    pub use verald_host::{
        ClockPort, HostError, InputOnly, InputSignal, ModportRead, ResetPort, Signals, failed, time,
    };
}
