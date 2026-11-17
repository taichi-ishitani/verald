//! Clocking blocks: signals read and written as plain struct fields.
//!
//! A `#[modport]` struct declares the members of an interface's modport as
//! fields: `#[input]` fields are sampled, `#[output]` fields are driven. A
//! `#[clocking]` struct holds modports and a clock, as SystemVerilog's
//! `clocking cb @(posedge clk)`. `tick!(cb)` drives the outputs changed since
//! the last tick, waits for the clock's next edge, and samples the inputs.
//!
//! ```ignore
//! #[modport]
//! struct AdderBus {
//!     #[output] start: bool,
//!     #[output] a: u8,
//!     #[input]  done: bool,
//!     #[input]  result: u16,
//! }
//!
//! #[clocking]
//! struct AdderCb {
//!     #[modport] bus: AdderBus,
//! }
//!
//! let mut cb = AdderCb::new(start.clock("clk")?, start.interface("bus")?)?;
//! cb.bus.start = true;
//! tick!(cb);
//! posedge!(cb.bus.done);
//! ```
//!
//! The wait macros (`tick_until!`, `posedge!`, ..) read, while they wait,
//! only the inputs their condition refers to, and every input at the edge the
//! wait ends; the wait methods (`cb.tick_until(|c| ..)`), whose closures
//! cannot be looked into, read every input at each edge.

use std::fmt;
use std::future::{Future, IntoFuture};
use std::pin::Pin;

use num_bigint::BigUint;
use smallvec::SmallVec;
use veryl_component::{InputPort, ModportRef, OutputPort, Value};

use crate::{Clock, TestError, sim};

/// A Rust type a signal is read into and written from.
///
/// Implemented for `bool`, the unsigned integers up to `u128`, `BigUint` (any
/// width) and `Value` (the bits as they are, with X and Z). Every type but
/// `Value` reads X and Z bits as 0, as SystemVerilog converts to two-state
/// types.
pub trait SignalValue: Clone + PartialEq {
    /// Whether a signal of `width` bits fits the type.
    fn fits(width: u32) -> bool;
    /// All zeros, for a signal of `width` bits: a field's value before its
    /// first sample.
    fn zero(width: u32) -> Self {
        Self::from_value(&Value::from_u64(0, width))
    }
    /// The value of the bits read.
    fn from_value(value: &Value) -> Self;
    /// The bits to write to a signal of `width` bits.
    fn to_value(&self, width: u32) -> Value;
}

/// The bits of `value` with X and Z read as 0, least significant word first.
fn two_state(value: &Value) -> impl Iterator<Item = u64> + '_ {
    let mask = value.mask_xz();
    value
        .as_slice()
        .iter()
        .enumerate()
        .map(move |(i, w)| w & !mask.get(i).copied().unwrap_or(0))
}

impl SignalValue for bool {
    fn fits(width: u32) -> bool {
        width == 1
    }
    fn from_value(value: &Value) -> Self {
        two_state(value).next().unwrap_or(0) & 1 != 0
    }
    fn to_value(&self, width: u32) -> Value {
        Value::from_u64(u64::from(*self), width)
    }
}

macro_rules! unsigned_signal_value {
    ($($ty:ty),*) => {$(
        impl SignalValue for $ty {
            fn fits(width: u32) -> bool {
                width <= <$ty>::BITS
            }
            fn from_value(value: &Value) -> Self {
                two_state(value).next().unwrap_or(0) as $ty
            }
            fn to_value(&self, width: u32) -> Value {
                Value::from_u64(u64::from(*self), width)
            }
        }
    )*};
}

unsigned_signal_value!(u8, u16, u32, u64);

impl SignalValue for u128 {
    fn fits(width: u32) -> bool {
        width <= 128
    }
    fn from_value(value: &Value) -> Self {
        let mut words = two_state(value);
        let lo = words.next().unwrap_or(0);
        let hi = words.next().unwrap_or(0);
        u128::from(lo) | (u128::from(hi) << 64)
    }
    fn to_value(&self, width: u32) -> Value {
        let words: SmallVec<[u64; 2]> = SmallVec::from_slice(&[*self as u64, (*self >> 64) as u64]);
        Value::from_bits(words, SmallVec::new(), width)
    }
}

impl SignalValue for BigUint {
    fn fits(_width: u32) -> bool {
        true
    }
    fn from_value(value: &Value) -> Self {
        let words: Vec<u64> = two_state(value).collect();
        let digits: Vec<u32> = words
            .iter()
            .flat_map(|w| [*w as u32, (*w >> 32) as u32])
            .collect();
        BigUint::from_slice(&digits)
    }
    fn to_value(&self, width: u32) -> Value {
        Value::from_bits(
            SmallVec::from_vec(self.to_u64_digits()),
            SmallVec::new(),
            width,
        )
    }
}

impl SignalValue for Value {
    fn fits(_width: u32) -> bool {
        true
    }
    fn from_value(value: &Value) -> Self {
        value.clone()
    }
    fn to_value(&self, width: u32) -> Value {
        let words = SmallVec::from_slice(self.as_slice());
        let mask = SmallVec::from_slice(self.mask_xz());
        Value::from_bits(words, mask, width)
    }
}

#[derive(Clone, Copy)]
enum MemberPort {
    Input(InputPort),
    Output(OutputPort),
}

/// A field of a modport, bound to its signal. Generated code holds one per
/// field.
#[doc(hidden)]
pub struct Member<T> {
    port: MemberPort,
    width: u32,
    /// The value last driven, for an output: unchanged outputs are not
    /// written again.
    last: T,
}

impl<T> fmt::Debug for Member<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Member")
    }
}

fn check_width<T: SignalValue>(what: &str, width: u32) -> Result<(), TestError> {
    if T::fits(width) {
        Ok(())
    } else {
        Err(TestError::new(format!(
            "`{what}` is {width} bits wide, which does not fit `{}`",
            std::any::type_name::<T>()
        )))
    }
}

impl<T: SignalValue> Member<T> {
    /// Binds the input member `name` of `bus`, checking that it fits `T`.
    pub fn input(bus: &ModportRef, name: &str) -> Result<Self, TestError> {
        Self::input_port(bus.input(name)?, &format!("{}.{name}", bus.path()))
    }

    /// Binds the input signal `port`; `what` names it in an error.
    pub fn input_port(port: InputPort, what: &str) -> Result<Self, TestError> {
        check_width::<T>(what, port.width())?;
        Ok(Member {
            port: MemberPort::Input(port),
            width: port.width(),
            last: T::zero(port.width()),
        })
    }

    /// Binds the output member `name` of `bus`, checking that it fits `T`.
    pub fn output(bus: &ModportRef, name: &str) -> Result<Self, TestError> {
        Self::output_port(bus.output(name)?, &format!("{}.{name}", bus.path()))
    }

    /// Binds the output signal `port`; `what` names it in an error.
    pub fn output_port(port: OutputPort, what: &str) -> Result<Self, TestError> {
        check_width::<T>(what, port.width())?;
        Ok(Member {
            port: MemberPort::Output(port),
            width: port.width(),
            last: T::zero(port.width()),
        })
    }

    /// The field's value before its first sample: all zeros.
    pub fn initial(&self) -> T {
        T::zero(self.width)
    }

    /// The value of an input before the current edge.
    pub fn load(&self) -> T {
        match self.port {
            MemberPort::Input(port) => T::from_value(&sim::read(port)),
            _ => unreachable!("loading a member that is not an input"),
        }
    }

    /// Takes `value` as the value last driven, without driving it.
    pub fn assume(&mut self, value: &T) {
        self.last = value.clone();
    }

    /// Drives `value`, whatever was last driven.
    pub fn force(&mut self, value: &T) {
        match self.port {
            MemberPort::Output(port) => sim::write(port, value.to_value(self.width)),
            _ => unreachable!("storing a member that is not an output"),
        }
        self.last = value.clone();
    }

    /// Drives `value` if it differs from the value last driven.
    pub fn store(&mut self, value: &T) {
        if *value != self.last {
            match self.port {
                MemberPort::Output(port) => sim::write(port, value.to_value(self.width)),
                _ => unreachable!("storing a member that is not an output"),
            }
            self.last = value.clone();
        }
    }
}

/// Where a clocking block's fields bind: the interfaces and signals the test
/// module registered. `Start` implements it, and so does the methodology's
/// `Scope`.
pub trait Signals {
    /// The clock registered as `name` (an interface's clock member as
    /// `"bus.clk"`).
    fn clock(&self, name: &str) -> Result<Clock, TestError>;
    /// The interface registered as `name`.
    fn interface(&self, name: &str) -> Result<ModportRef, TestError>;
    /// The signal read at `path`: a registered input, or an interface's
    /// member (`"bus.done"`).
    fn input(&self, path: &str) -> Result<InputPort, TestError>;
    /// The signal driven at `path`: a registered output, or an interface's
    /// member (`"bus.start"`).
    fn output(&self, path: &str) -> Result<OutputPort, TestError>;

    /// Makes a clocking block following the clock registered as `clock`,
    /// every field bound at its own name:
    /// `scope.clocking::<AdderCb>("clk")?`.
    fn clocking<C: Clocking>(&self, clock: &str) -> Result<C, TestError>
    where
        Self: Sized,
    {
        C::bind(self, self.clock(clock)?, Default::default())
    }

    /// Makes a clocking block following the clock registered as `clock`,
    /// with fields bound where `paths` says; the block's type follows from
    /// the paths' type:
    /// `scope.clocking_with("clk", AdderCbPaths { count: Some("cnt"), ..Default::default() })?`.
    fn clocking_with<P: ClockingPaths>(&self, clock: &str, paths: P) -> Result<P::Block, TestError>
    where
        Self: Sized,
    {
        paths.bind(self, self.clock(clock)?)
    }
}

/// A clocking block, as `#[clocking]` makes one: how to bind it.
pub trait Clocking: Sized {
    /// The struct overriding where its fields bind (`<Name>Paths`).
    type Paths<'a>: Default;
    /// Binds the block, as its `new` does.
    fn bind<S: Signals + ?Sized>(
        signals: &S,
        clock: Clock,
        paths: Self::Paths<'_>,
    ) -> Result<Self, TestError>;
}

/// The paths struct of a clocking block (`<Name>Paths`), which knows its
/// block.
pub trait ClockingPaths {
    type Block;
    /// Binds the block at these paths.
    fn bind<S: Signals + ?Sized>(self, signals: &S, clock: Clock)
    -> Result<Self::Block, TestError>;
}

/// Implemented by `#[modport]` for its struct.
#[doc(hidden)]
pub trait Modport: Sized {
    fn __bind(bus: &ModportRef) -> Result<Self, TestError>;
    fn __load(&mut self);
    fn __store(&mut self);
    fn __init(&mut self);
}

/// Implemented by `#[clocking]` for its struct.
#[doc(hidden)]
pub trait ClockingBlock {
    fn __clock(&self) -> Clock;
    fn __load(&mut self);
    fn __store(&mut self);
}

/// Waits for the next edge of a clocking block's clock, like
/// SystemVerilog's `@(cb)`: drives the outputs assigned since the last tick,
/// waits for the edge, and samples every input. It stands for
/// `cb.tick().await`.
#[macro_export]
macro_rules! tick {
    ($cb:expr) => {
        ($cb).tick().await
    };
}

/// Drives the changed outputs now, without waiting for an edge.
#[doc(hidden)]
pub fn drive<C: ClockingBlock>(c: &mut C) {
    c.__store();
}

/// Drives the changed outputs, waits for the next edge and samples every
/// input.
#[doc(hidden)]
pub async fn tick<C: ClockingBlock>(c: &mut C) {
    c.__store();
    c.__clock().edge().await;
    c.__load();
}

type Hit<'a, C> = Box<dyn FnMut(&C) -> bool + 'a>;
type Sample<'a, C> = Box<dyn FnMut(&mut C) + 'a>;

/// A wait on a clocking block (`posedge`, `negedge`, `edge`, `changed`,
/// `wait`, `tick_until`, `tick_while`). Await it, or give it an edge limit
/// with [`Wait::within`].
#[must_use = "a wait does nothing unless awaited"]
pub struct Wait<'a, C> {
    c: &'a mut C,
    /// Whether a condition already met ends the wait without a tick.
    immediate: bool,
    /// Called on the samples of each edge (and before the first one when
    /// `immediate`); true ends the wait.
    hit: Hit<'a, C>,
    /// Reads, at each edge while waiting, what `hit` needs; `None` reads
    /// every input.
    sample: Option<Sample<'a, C>>,
    label: Option<String>,
}

impl<'a, C: ClockingBlock + 'a> Wait<'a, C> {
    #[doc(hidden)]
    pub fn new(c: &'a mut C, immediate: bool, hit: Hit<'a, C>) -> Self {
        Self {
            c,
            immediate,
            hit,
            sample: None,
            label: None,
        }
    }

    /// Reads only what `sample` reads while waiting; every input is read at
    /// the edge the wait ends. Used by the wait macros.
    #[doc(hidden)]
    pub fn sampling(mut self, sample: impl FnMut(&mut C) + 'a) -> Self {
        self.sample = Some(Box::new(sample));
        self
    }

    /// Names the wait in the timeout error of [`Wait::within`].
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Gives up after `edges` clock edges, returning [`ClockingTimeout`].
    pub fn within(self, edges: u64) -> WaitWithin<'a, C> {
        WaitWithin { wait: self, edges }
    }

    async fn run(mut self, limit: Option<u64>) -> Result<(), ClockingTimeout> {
        if self.immediate && (self.hit)(self.c) {
            return Ok(());
        }
        let clock = self.c.__clock();
        let mut edges = 0;
        loop {
            if limit.is_some_and(|limit| edges >= limit) {
                return Err(ClockingTimeout {
                    label: self.label,
                    edges,
                });
            }
            self.c.__store();
            clock.edge().await;
            edges += 1;
            match &mut self.sample {
                Some(sample) => sample(self.c),
                None => self.c.__load(),
            }
            if (self.hit)(self.c) {
                if self.sample.is_some() {
                    self.c.__load();
                }
                return Ok(());
            }
        }
    }
}

impl<'a, C: ClockingBlock + 'a> IntoFuture for Wait<'a, C> {
    type Output = ();
    type IntoFuture = Pin<Box<dyn Future<Output = ()> + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            // Without a limit the wait cannot time out.
            let _ = self.run(None).await;
        })
    }
}

/// A [`Wait`] limited to a number of clock edges.
#[must_use = "a wait does nothing unless awaited"]
pub struct WaitWithin<'a, C> {
    wait: Wait<'a, C>,
    edges: u64,
}

impl<'a, C: ClockingBlock + 'a> WaitWithin<'a, C> {
    /// Names the wait in its timeout error.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.wait.label = Some(label.into());
        self
    }
}

impl<'a, C: ClockingBlock + 'a> IntoFuture for WaitWithin<'a, C> {
    type Output = Result<(), ClockingTimeout>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.wait.run(Some(self.edges)))
    }
}

/// A wait limited by [`Wait::within`] saw no match within its edges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockingTimeout {
    pub label: Option<String>,
    pub edges: u64,
}

impl fmt::Display for ClockingTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.label {
            Some(label) => write!(f, "`{label}` not seen within {} cycles", self.edges),
            None => write!(f, "condition not seen within {} cycles", self.edges),
        }
    }
}

impl std::error::Error for ClockingTimeout {}

impl From<ClockingTimeout> for TestError {
    fn from(e: ClockingTimeout) -> Self {
        TestError::new(e.to_string())
    }
}

/// The wait constructors the `#[clocking]` methods call.
#[doc(hidden)]
pub mod waits {
    use super::{ClockingBlock, Wait};

    pub fn posedge<'a, C: ClockingBlock + 'a>(
        c: &'a mut C,
        mut f: impl FnMut(&C) -> bool + 'a,
    ) -> Wait<'a, C> {
        let mut prev = f(c);
        Wait::new(
            c,
            false,
            Box::new(move |c| {
                let now = f(c);
                let hit = !prev && now;
                prev = now;
                hit
            }),
        )
    }

    pub fn negedge<'a, C: ClockingBlock + 'a>(
        c: &'a mut C,
        mut f: impl FnMut(&C) -> bool + 'a,
    ) -> Wait<'a, C> {
        let mut prev = f(c);
        Wait::new(
            c,
            false,
            Box::new(move |c| {
                let now = f(c);
                let hit = prev && !now;
                prev = now;
                hit
            }),
        )
    }

    pub fn edge<'a, C: ClockingBlock + 'a>(
        c: &'a mut C,
        f: impl FnMut(&C) -> bool + 'a,
    ) -> Wait<'a, C> {
        changed(c, f)
    }

    pub fn changed<'a, C: ClockingBlock + 'a, T: PartialEq + 'a>(
        c: &'a mut C,
        mut f: impl FnMut(&C) -> T + 'a,
    ) -> Wait<'a, C> {
        let mut prev = f(c);
        Wait::new(
            c,
            false,
            Box::new(move |c| {
                let now = f(c);
                let hit = now != prev;
                prev = now;
                hit
            }),
        )
    }

    pub fn wait<'a, C: ClockingBlock + 'a>(
        c: &'a mut C,
        f: impl FnMut(&C) -> bool + 'a,
    ) -> Wait<'a, C> {
        Wait::new(c, true, Box::new(f))
    }

    pub fn tick_until<'a, C: ClockingBlock + 'a>(
        c: &'a mut C,
        f: impl FnMut(&C) -> bool + 'a,
    ) -> Wait<'a, C> {
        Wait::new(c, false, Box::new(f))
    }

    pub fn tick_while<'a, C: ClockingBlock + 'a>(
        c: &'a mut C,
        mut f: impl FnMut(&C) -> bool + 'a,
    ) -> Wait<'a, C> {
        Wait::new(c, false, Box::new(move |c| !f(c)))
    }
}

/// Items the generated code names, independent of the user's imports.
#[doc(hidden)]
pub mod __private {
    pub use super::Signals;
    pub use crate::{Clock, TestError};
    pub use veryl_component::{InputPort, ModportRef, OutputPort};
}
