//! Signals read and written through the host layer's direct paths: in a
//! two-state run they give the values the SDK's `read` / `write` give, and
//! in a four-state run X and Z read as 0.

use std::cell::RefCell;

use smallvec::SmallVec;
use verald_tb_runtime::prelude::*;
use verald_tb_runtime::{BigUint, Start};
use veryl_component::Value;
use veryl_component::testing::{MockSim, RefDir};

/// One field of each type, as inputs and as outputs.
#[modport]
struct Values {
    #[input]
    b: bool,
    #[input]
    n8: u8,
    #[input]
    n64: u64,
    #[input]
    n100: u128,
    #[input]
    n200: BigUint,
    #[output]
    o1: bool,
    #[output]
    o8: u8,
    #[output]
    o64: u64,
    #[output]
    o100: u128,
    #[output]
    o200: BigUint,
}

#[clocking]
struct Cb {
    #[modport]
    bus: Values,
}

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

struct Tests;

impl Testbench for Tests {
    fn start(start: &mut Start) -> Result<(), TestError> {
        let mut cb = start.clocking::<Cb>("clk")?;
        start.main("test", async move {
            tick!(cb);
            let bus = &cb.bus;
            let line = format!(
                "{} {:#x} {:#x} {:#x} {:#x}",
                bus.b, bus.n8, bus.n64, bus.n100, bus.n200
            );
            LOG.with(|l| l.borrow_mut().push(line));
            // Echo the inputs on the outputs.
            cb.bus.o1 = cb.bus.b;
            cb.bus.o8 = cb.bus.n8;
            cb.bus.o64 = cb.bus.n64;
            cb.bus.o100 = cb.bus.n100;
            cb.bus.o200 = cb.bus.n200.clone();
            tick!(cb);
            Ok(())
        });
        Ok(())
    }
}

verald_tb_runtime::component!(Tb, Tests);

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn bits(words: &[u64], mask: &[u64], width: u32) -> Value {
    Value::from_bits(
        SmallVec::from_slice(words),
        SmallVec::from_slice(mask),
        width,
    )
}

const WIDTHS: [(&str, u32); 5] = [
    ("b", 1),
    ("n8", 8),
    ("n64", 64),
    ("n100", 100),
    ("n200", 200),
];
const OUTPUTS: [(&str, u32); 5] = [
    ("o1", 1),
    ("o8", 8),
    ("o64", 64),
    ("o100", 100),
    ("o200", 200),
];

fn setup(four_state: bool) -> (MockSim, Tb) {
    take_log();
    let mut sim = MockSim::new().four_state(four_state);
    let mut tb = sim.build::<Tb>().unwrap();
    sim.init(&mut tb).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut tb, "set_clock", &[str("clk"), clk]).unwrap();
    let dims: Vec<[u32; 1]> = WIDTHS.iter().chain(&OUTPUTS).map(|(_, w)| [*w]).collect();
    let members: Vec<(&str, RefDir, &[u32])> = WIDTHS
        .iter()
        .map(|(n, _)| (*n, RefDir::Input))
        .chain(OUTPUTS.iter().map(|(n, _)| (*n, RefDir::Output)))
        .zip(&dims)
        .map(|((n, dir), dims)| (n, dir, &dims[..]))
        .collect();
    let bus = sim.modport_ref("bus", "values_if", "tb", &members);
    sim.call(&mut tb, "set_interface", &[str("bus"), bus])
        .unwrap();
    sim.call(&mut tb, "start", &[str("values")]).unwrap();
    (sim, tb)
}

fn clock(sim: &mut MockSim, tb: &mut Tb) {
    sim.set_fired_clock("clk");
    sim.clock(tb).unwrap();
}

/// What the SDK's `read` gives for `value`, X and Z as 0, as a hex string.
fn two_state_hex(value: &Value) -> String {
    let mask = value.mask_xz();
    let words: Vec<u64> = value
        .as_slice()
        .iter()
        .enumerate()
        .map(|(i, w)| w & !mask.get(i).copied().unwrap_or(0))
        .collect();
    let digits: Vec<u32> = words
        .iter()
        .flat_map(|w| [*w as u32, (*w >> 32) as u32])
        .collect();
    format!("{:#x}", BigUint::from_slice(&digits))
}

fn run(four_state: bool, inputs: [Value; 5]) {
    let (mut sim, mut tb) = setup(four_state);
    for ((name, _), value) in WIDTHS.iter().zip(&inputs) {
        sim.set(&format!("bus.{name}"), value.clone());
    }
    clock(&mut sim, &mut tb);
    let expected: Vec<String> = inputs.iter().map(two_state_hex).collect();
    let b = if expected[0] == "0x1" {
        "true"
    } else {
        "false"
    };
    assert_eq!(
        take_log(),
        [format!(
            "{b} {} {} {} {}",
            expected[1], expected[2], expected[3], expected[4]
        )]
    );
    // The outputs carry the two-state values, at their widths, with no X or
    // Z driven: what the SDK's `write` of those values gives.
    for ((name, width), hex) in OUTPUTS.iter().zip(&expected) {
        let out = sim.get(&format!("bus.{name}"));
        assert!(!out.has_x(), "`{name}` drives X or Z");
        assert_eq!(two_state_hex(&out), *hex, "`{name}`");
        assert_eq!(
            out.as_slice().len(),
            (*width as usize).div_ceil(64),
            "`{name}`"
        );
    }
}

#[test]
fn two_state_reads_and_writes_give_the_sdks_values() {
    run(
        false,
        [
            Value::from_u64(1, 1),
            Value::from_u64(0xa5, 8),
            Value::from_u64(0x8000_0000_0000_0001, 64),
            bits(&[0x0123_4567_89ab_cdef, 0xf_edcb_a987], &[0, 0], 100),
            bits(&[1, 2, 3, 0xff], &[0, 0, 0, 0], 200),
        ],
    );
}

#[test]
fn four_state_reads_give_x_and_z_as_zero() {
    // A set mask bit is X where the payload is 0 and Z where it is 1.
    run(
        true,
        [
            bits(&[1], &[1], 1),
            bits(&[0xa5], &[0x0f], 8),
            bits(&[u64::MAX], &[0xffff_0000_0000_0000], 64),
            bits(&[u64::MAX, 0xf_ffff_ffff], &[0xff, 0xf_0000_0000], 100),
            bits(&[1, 2, 3, 0xff], &[1, 0, 0, 0xf0], 200),
        ],
    );
}
