//! Named sequences (`#[sequence]`) and properties (`#[property]`),
//! expanded where they are used.

mod common;

use common::summary;
use verald_assertion::{Assertions, assertions, blocks};

#[assertions]
#[derive(Default)]
struct Bridge {
    #[clock]
    clk: Clock,
    #[input]
    i_valid: bool,
    #[input]
    i_ready: bool,
    #[input]
    i_data: u8,
    #[input]
    o_valid: bool,
    #[input]
    o_ready: bool,
    #[input]
    o_data: u8,
    #[input]
    a: bool,
}

#[assertions]
impl Bridge {
    /// SVA's `sequence handshake(valid, ready)`.
    #[sequence]
    fn handshake(valid: bool, ready: bool) {
        seq!(valid -> wait(ready));
    }

    /// A value, and the same value at the output the edge after.
    #[sequence]
    fn echoed(value: u8) {
        seq!(let d: u8 = value -> delay(1) -> o_data == d);
    }

    /// SVA's `property stable_until_ready(valid, ready, data)`.
    #[property]
    fn stable_until_ready(valid: bool, ready: bool, data: u8) {
        if valid && !ready {
            seq!(delay(1) -> valid && stable(data));
        }
    }

    /// A property using another.
    #[property]
    fn input_held() {
        stable_until_ready(i_valid, i_ready, i_data);
    }

    #[assert]
    fn req_hold(&self) {
        input_held();
    }

    #[assert]
    fn rsp_hold(&self) {
        stable_until_ready(o_valid, o_ready, o_data);
    }

    /// After a handshake, `o_valid` the edge after.
    #[assert]
    fn out_after_handshake(&self) {
        if seq!(handshake(i_valid, i_ready) -> delay(1)) {
            o_valid;
        }
    }

    /// The caller's `d` is not the sequence's own.
    #[assert]
    fn echo_plus_one(&self) {
        if a {
            let d: u8 = i_data;
            seq!(echoed(d + 1));
        }
    }
}

verald_host::component! {
    struct Comp { assertions: Assertions<blocks![Bridge]> }

    fn start(&mut self, ctx: &mut ::veryl_component::SimCtx) -> ::veryl_component::Result<()> {
        verald_host::entry::start(ctx, None, |test| self.__verald_on_start(test))
    }

    fn bind_to_instance(
        &mut self,
        _ctx: &mut ::veryl_component::SimCtx,
        path: String,
        block: String,
    ) -> ::veryl_component::Result<()> {
        verald_host::entry::register(self.assertions.bind_to_instance(path, block))
    }
}

const SIGNALS: &[(&str, u32)] = &[
    ("i_valid", 1),
    ("i_ready", 1),
    ("i_data", 8),
    ("o_valid", 1),
    ("o_ready", 1),
    ("o_data", 8),
    ("a", 1),
];

fn run(columns: &[&str], rows: &[&[u64]]) -> Vec<String> {
    common::run::<Comp>(SIGNALS, "Bridge", columns, rows)
}

#[test]
fn a_named_property_applies_to_its_arguments() {
    // The input side: held while stalled.
    let cols = &["i_valid", "i_ready", "i_data", "o_valid"];
    assert!(run(cols, &[&[1, 0, 5, 0], &[1, 1, 5, 0], &[0, 0, 0, 1]]).is_empty());
    let failures = run(cols, &[&[1, 0, 5, 0], &[1, 1, 6, 0], &[0, 0, 0, 1]]);
    let summary = summary(&failures);
    assert_eq!(summary.len(), 1, "{failures:?}");
    assert_eq!((summary[0].0, summary[0].1), (2, Some(1)));
    assert!(
        failures[0].contains("`Bridge.req_hold`") && summary[0].2.contains("stable"),
        "{failures:?}"
    );
    // The output side, through the same property.
    let cols = &["o_valid", "o_ready", "o_data"];
    let failures = run(cols, &[&[1, 0, 5], &[0, 0, 5]]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("`Bridge.rsp_hold`"), "{failures:?}");
}

#[test]
fn a_named_sequence_is_an_element() {
    let cols = &["i_valid", "i_ready", "o_valid"];
    // Handshake at 2 (`valid` at 1, `ready` at 2), `o_valid` at 3.
    assert!(run(cols, &[&[1, 0, 0], &[1, 1, 0], &[0, 0, 1]]).is_empty());
    let failures = run(cols, &[&[1, 0, 0], &[1, 1, 0], &[0, 0, 0]]);
    assert_eq!(
        summary(&failures),
        vec![(3, Some(1), "o_valid".to_string())],
        "{failures:?}"
    );
}

#[test]
fn a_named_sequences_let_does_not_capture_the_callers() {
    let cols = &["a", "i_data", "o_data"];
    assert!(run(cols, &[&[1, 4, 0], &[0, 0, 5]]).is_empty());
    let failures = run(cols, &[&[1, 4, 0], &[0, 0, 4]]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("`Bridge.echo_plus_one`"),
        "{failures:?}"
    );
}
