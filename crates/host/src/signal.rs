//! The layer's types for signals: values, typed signals, and where signals
//! come from. Modports are the component SDK's (`#[modport]`, `Modport`).

use std::fmt;

use num_bigint::BigUint;
use smallvec::SmallVec;
use veryl_component::{ClockPort, InputPort, OutputPort, ResetPort};

use super::HostError;

/// A Rust type a signal is read into and written from. Each type reads
/// and writes itself, taking the shortest path for it: `bool` and the
/// integers up to 64 bits through one word, wider ones through words, never
/// through a `Value`. Signals are two-state: X and Z read as 0, as
/// SystemVerilog converts to two-state types, and nothing drives X or Z.
///
/// Implemented for `bool`, the unsigned integers up to `u128` and
/// `BigUint` (any width).
pub trait SignalValue: Clone + PartialEq + Default {
    /// Whether a signal of `width` bits fits the type; checked when bound.
    fn fits(width: u32) -> bool;
    /// The port's value before the current edge.
    fn read(port: InputPort) -> Self;
    /// Drives the port; the value reaches the design at the edge.
    fn write(&self, port: OutputPort);
}

impl SignalValue for bool {
    fn fits(width: u32) -> bool {
        width == 1
    }
    fn read(port: InputPort) -> Self {
        super::read_u64(port) & 1 != 0
    }
    fn write(&self, port: OutputPort) {
        super::write_u64(port, u64::from(*self));
    }
}

macro_rules! unsigned_signal_value {
    ($($ty:ty),*) => {$(
        impl SignalValue for $ty {
            fn fits(width: u32) -> bool {
                width <= <$ty>::BITS
            }
            fn read(port: InputPort) -> Self {
                super::read_u64(port) as $ty
            }
            fn write(&self, port: OutputPort) {
                super::write_u64(port, u64::from(*self));
            }
        }
    )*};
}

unsigned_signal_value!(u8, u16, u32, u64);

impl SignalValue for u128 {
    fn fits(width: u32) -> bool {
        width <= 128
    }
    fn read(port: InputPort) -> Self {
        let mut words = [0u64; 2];
        super::read_words(port, &mut words);
        u128::from(words[0]) | (u128::from(words[1]) << 64)
    }
    fn write(&self, port: OutputPort) {
        super::write_words(port, &[*self as u64, (*self >> 64) as u64]);
    }
}

impl SignalValue for BigUint {
    fn fits(_width: u32) -> bool {
        true
    }
    fn read(port: InputPort) -> Self {
        let mut words: SmallVec<[u64; 4]> = SmallVec::from_elem(0, super::words_for(port.width()));
        super::read_words(port, &mut words);
        let digits: SmallVec<[u32; 8]> = words
            .iter()
            .flat_map(|w| [*w as u32, (*w >> 32) as u32])
            .collect();
        BigUint::from_slice(&digits)
    }
    fn write(&self, port: OutputPort) {
        let mut words: SmallVec<[u64; 4]> = self.iter_u64_digits().collect();
        words.resize(super::words_for(port.width()), 0);
        super::write_words(port, &words);
    }
}

fn check_width<T: SignalValue>(what: &str, width: u32) -> Result<(), HostError> {
    if T::fits(width) {
        Ok(())
    } else {
        Err(HostError::new(format!(
            "`{what}` is {width} bits wide, which does not fit `{}`",
            std::any::type_name::<T>()
        )))
    }
}

/// An input signal read as `T`: the port and its width, checked against `T`
/// when bound. It only reads.
pub struct InputSignal<T> {
    port: InputPort,
    value: std::marker::PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for InputSignal<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InputSignal")
    }
}

impl<T: SignalValue> InputSignal<T> {
    /// Binds the input `port`, checking that it fits `T`; `what` names it
    /// in an error.
    pub fn bind(port: InputPort, what: &str) -> Result<Self, HostError> {
        check_width::<T>(what, port.width())?;
        Ok(InputSignal {
            port,
            value: std::marker::PhantomData,
        })
    }

    /// The value before the first sample: all zeros.
    pub fn zero(&self) -> T {
        T::default()
    }

    /// The value before the current edge.
    pub fn read(&self) -> T {
        T::read(self.port)
    }
}

/// An output signal written from `T`: the port and its width, checked
/// against `T` when bound. It only writes; writing only what changed is the
/// caller's (clocking's `Driven`).
pub struct OutputSignal<T> {
    port: OutputPort,
    value: std::marker::PhantomData<fn(&T)>,
}

impl<T> fmt::Debug for OutputSignal<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutputSignal")
    }
}

impl<T: SignalValue> OutputSignal<T> {
    /// Binds the output `port`, checking that it fits `T`; `what` names it
    /// in an error.
    pub fn bind(port: OutputPort, what: &str) -> Result<Self, HostError> {
        check_width::<T>(what, port.width())?;
        Ok(OutputSignal {
            port,
            value: std::marker::PhantomData,
        })
    }

    /// All zeros.
    pub fn zero(&self) -> T {
        T::default()
    }

    /// Drives `value`; it reaches the design at the edge.
    pub fn write(&self, value: &T) {
        value.write(self.port);
    }
}

/// Where names bind, resolved to ports only. Which names exist is decided
/// by the implementer: the runtime's registrations (`Start`, the
/// methodology's `Scope`), later an instance's ports and observed signals.
pub trait Signals {
    /// What a modport's name resolves to: the component SDK's member
    /// source, which its `Modport::bind` takes.
    type Members: veryl_component::MemberSource;
    /// The modport registered as `name`.
    fn modport(&self, name: &str) -> Result<Self::Members, HostError>;
    /// The signal read at `path`: a signal of its own, or an interface's
    /// member (`"bus.done"`).
    fn input(&self, path: &str) -> Result<InputPort, HostError>;
    /// The signal driven at `path`: a signal of its own, or an interface's
    /// member (`"bus.start"`).
    fn output(&self, path: &str) -> Result<OutputPort, HostError>;
    /// The clock at `name` (an interface's clock member as `"bus.clk"`).
    fn clock_port(&self, name: &str) -> Result<ClockPort, HostError>;
    /// The reset at `name` (an interface's reset member as `"bus.rst"`).
    fn reset_port(&self, name: &str) -> Result<ResetPort, HostError>;
}
