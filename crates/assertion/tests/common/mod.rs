//! Running assertion blocks on `MockSim` from tables of the signals'
//! values at each edge, for the tests of temporal properties.

#![allow(dead_code)]

use veryl_component::testing::MockSim;
use veryl_component::{Component, Value};

pub fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

/// A simulator with `clk`, `rst` and `signals` registered, and `block`
/// bound at the root and started.
pub fn setup<C: Component>(signals: &[(&str, u32)], block: &str) -> (MockSim, C) {
    let mut sim = MockSim::new();
    let mut comp = sim.build::<C>().unwrap();
    sim.init(&mut comp).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut comp, "set_clock", &[str("clk"), clk])
        .unwrap();
    let rst = sim.reset_ref("rst");
    sim.call(&mut comp, "set_reset", &[str("rst"), rst])
        .unwrap();
    for (name, width) in signals {
        let sig = sim.signal_ref(name, false, &[*width]);
        sim.call(&mut comp, "set_input", &[str(name), sig]).unwrap();
    }
    sim.call(&mut comp, "bind_to_instance", &[str(""), str(block)])
        .unwrap();
    sim.call(&mut comp, "start", &[]).unwrap();
    (sim, comp)
}

pub fn set(sim: &mut MockSim, signals: &[(&str, u32)], name: &str, value: u64) {
    let width = signals.iter().find(|(n, _)| *n == name).unwrap().1;
    sim.set(name, Value::from_u64(value, width));
}

/// Binds `block` and runs one edge per row, at times 1, 2, ..; each row
/// gives the values of `columns` before that edge. Returns the failures.
pub fn run<C: Component>(
    signals: &[(&str, u32)],
    block: &str,
    columns: &[&str],
    rows: &[&[u64]],
) -> Vec<String> {
    let (sim, _) = edges::<C>(signals, block, columns, rows);
    sim.failures().to_vec()
}

/// As [`run`], then ends the test: returns the failures and the lines
/// logged (the attempts reported incomplete among them).
pub fn run_to_end<C: Component>(
    signals: &[(&str, u32)],
    block: &str,
    columns: &[&str],
    rows: &[&[u64]],
) -> (Vec<String>, Vec<String>) {
    let (mut sim, mut comp) = edges::<C>(signals, block, columns, rows);
    sim.time = rows.len() as u64 + 1;
    sim.finish(&mut comp).unwrap();
    (sim.failures().to_vec(), sim.logs().to_vec())
}

fn edges<C: Component>(
    signals: &[(&str, u32)],
    block: &str,
    columns: &[&str],
    rows: &[&[u64]],
) -> (MockSim, C) {
    let (mut sim, mut comp) = setup::<C>(signals, block);
    sim.set_fired_clock("clk");
    for (t, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), columns.len());
        for (name, value) in columns.iter().zip(*row) {
            set(&mut sim, signals, name, *value);
        }
        sim.time = t as u64 + 1;
        sim.clock(&mut comp).unwrap();
    }
    (sim, comp)
}

/// The failures' times and expectations: `(at, started, expr)`, `started`
/// when it differs from `at`.
pub fn summary(failures: &[String]) -> Vec<(u64, Option<u64>, String)> {
    failures
        .iter()
        .map(|f| {
            let rest = f.split(" failed at ").nth(1).expect(f);
            let (at, rest) = rest.split_once([' ', ':']).expect(f);
            let started = rest
                .strip_prefix("(started at ")
                .map(|r| r.split([',', ')']).next().unwrap().parse().unwrap());
            let expr = rest.split('`').nth(1).expect(f).to_string();
            (at.parse().unwrap(), started, expr)
        })
        .collect()
}
