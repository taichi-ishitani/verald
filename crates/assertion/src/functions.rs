//! The functions on values that properties call: `onehot`, `onehot0`,
//! `countones`, and the least significant bit `rose` and `fell` look at.

use num_bigint::BigUint;

/// A signal's value as bits: what the functions on values need of it.
/// Implemented for the types of signal fields: `bool`, the unsigned
/// integers and `BigUint`, and for references to them: a `BigUint` is
/// passed by reference (`onehot(&grant)`).
pub trait Bits {
    /// The number of bits set.
    fn count_ones(&self) -> u64;
    /// The least significant bit.
    fn lsb(&self) -> bool;
}

impl Bits for bool {
    fn count_ones(&self) -> u64 {
        u64::from(*self)
    }
    fn lsb(&self) -> bool {
        *self
    }
}

macro_rules! unsigned_bits {
    ($($ty:ty),*) => {$(
        impl Bits for $ty {
            fn count_ones(&self) -> u64 {
                u64::from(<$ty>::count_ones(*self))
            }
            fn lsb(&self) -> bool {
                *self & 1 != 0
            }
        }
    )*};
}

unsigned_bits!(u8, u16, u32, u64, u128);

impl<T: Bits + ?Sized> Bits for &T {
    fn count_ones(&self) -> u64 {
        T::count_ones(self)
    }
    fn lsb(&self) -> bool {
        T::lsb(self)
    }
}

impl Bits for BigUint {
    fn count_ones(&self) -> u64 {
        BigUint::count_ones(self)
    }
    fn lsb(&self) -> bool {
        self.bit(0)
    }
}

/// Whether exactly one bit is set (SVA's `$onehot`).
pub fn onehot<T: Bits>(x: T) -> bool {
    x.count_ones() == 1
}

/// Whether at most one bit is set (SVA's `$onehot0`).
pub fn onehot0<T: Bits>(x: T) -> bool {
    x.count_ones() <= 1
}

/// The number of bits set (SVA's `$countones`).
pub fn countones<T: Bits>(x: T) -> u64 {
    x.count_ones()
}

/// The least significant bit, for `rose` and `fell`.
#[doc(hidden)]
pub fn lsb<T: Bits>(x: T) -> bool {
    x.lsb()
}
