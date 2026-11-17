//! The randomization's context: a configuration the constraints read
//! (`#[randomize(context = C)]`, `context.x`), given at each call.

use veryl_tbf_random::{
    RandErrorKind, Randomize, constraints, randomize, randomize_with, set_seed,
};

pub struct BusConfig {
    max_len: u8,
    align: Align,
}

pub struct Align {
    bytes: u16,
}

#[randomize(context = BusConfig)]
#[derive(Clone, Debug, Default)]
pub struct Burst {
    #[rand]
    len: u8,
    #[rand]
    addr: u16,
}

#[constraints]
impl Burst {
    fn len_limit() {
        len >= 1;
        len <= context.max_len;
    }
    fn aligned() {
        addr % context.align.bytes == 0;
        addr < { u32::from(context.max_len) * 1000 };
    }
}

#[test]
fn constraints_read_the_context_given() {
    set_seed(1);
    let small = BusConfig {
        max_len: 3,
        align: Align { bytes: 16 },
    };
    let mut burst = Burst::default();
    for _ in 0..50 {
        burst.randomize(&small).unwrap();
        assert!((1..=3).contains(&burst.len), "{burst:?}");
        assert_eq!(burst.addr % 16, 0, "{burst:?}");
        assert!(burst.addr < 3000, "{burst:?}");
    }
    let wide = BusConfig {
        max_len: 200,
        align: Align { bytes: 1 },
    };
    let mut long = false;
    for _ in 0..50 {
        burst.randomize(&wide).unwrap();
        long |= burst.len > 3;
    }
    assert!(long, "the second context's limit is in effect");
}

#[constraints(for Burst)]
struct Half;

#[constraints(for Burst)]
impl Half {
    fn half() {
        len * 2 <= context.max_len;
    }
}

#[test]
fn sets_and_inline_constraints_read_it_too() {
    set_seed(2);
    let cfg = BusConfig {
        max_len: 10,
        align: Align { bytes: 4 },
    };
    let mut burst = Burst::default();
    for _ in 0..30 {
        burst.randomize_with(Half, &cfg).unwrap();
        assert!(burst.len <= 5, "{burst:?}");
    }
    for _ in 0..30 {
        randomize_with!(burst, &cfg, {
            len == context.max_len - 1;
        })
        .unwrap();
        assert_eq!(burst.len, 9, "{burst:?}");
        assert_eq!(burst.addr % 4, 0, "{burst:?}");
    }
}

/// An object holding a burst, under the same context.
#[randomize(context = BusConfig)]
#[derive(Clone, Debug, Default)]
pub struct Transfer {
    #[rand]
    burst: Burst,
    #[rand]
    gap: u8,
}

#[constraints]
impl Transfer {
    fn short_gap() {
        gap < context.max_len;
    }
}

#[test]
fn a_nested_object_gets_its_parents_context() {
    set_seed(3);
    let cfg = BusConfig {
        max_len: 4,
        align: Align { bytes: 8 },
    };
    let mut t = Transfer::default();
    for _ in 0..30 {
        t.randomize(&cfg).unwrap();
        assert!(t.gap < 4 && (1..=4).contains(&t.burst.len), "{t:?}");
        assert_eq!(t.burst.addr % 8, 0, "{t:?}");
    }
}

/// A parent with no context, holding a burst that needs one.
#[randomize]
#[derive(Clone, Debug, Default)]
pub struct Orphan {
    #[rand]
    burst: Burst,
}

#[test]
fn a_context_not_given_fails_the_randomization() {
    set_seed(4);
    let mut o = Orphan::default();
    let e = o.randomize().unwrap_err();
    assert_eq!(e.kind(), RandErrorKind::Invalid);
    assert!(e.to_string().contains("BusConfig"), "{e}");
}

#[test]
fn the_trait_takes_the_context_explicitly() {
    set_seed(5);
    let cfg = BusConfig {
        max_len: 2,
        align: Align { bytes: 2 },
    };
    let mut burst = Burst::default();
    Randomize::randomize_in(&mut burst, &cfg).unwrap();
    assert!(burst.len <= 2);
}
