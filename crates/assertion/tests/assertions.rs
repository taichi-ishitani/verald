//! Assertion blocks in a component of their own, driven by
//! `veryl_component::testing::MockSim`.

use verald_assertion::{Assertions, assertions, blocks, modport};
use veryl_component::Value;
use veryl_component::testing::{MockSim, RefDir};

/// Properties over plain signals, with auxiliary state.
#[assertions]
#[derive(Default)]
struct ReqAssertions {
    #[clock]
    clk: Clock,
    #[reset]
    rst: Reset,
    #[input]
    req: bool,
    #[input]
    ack: bool,
    #[input]
    data: u8,
    // Auxiliary state: a request seen at the previous edge, and how long
    // requests have waited.
    pending: bool,
    waited: u32,
}

#[assertions]
impl ReqAssertions {
    fn waiting(&self) -> bool {
        req && !ack
    }

    /// An `ack` answers a request of the previous edge.
    #[assert]
    fn ack_answers_a_request(&self) {
        if ack {
            self.pending;
        }
    }

    #[assert]
    fn data_is_small(&self) {
        let limit = 200;
        data < limit;
    }

    #[on_clock]
    fn update(&mut self) {
        check!(self.waited < 3, "waited {} edges", self.waited);
        self.pending = req;
        if self.waiting() {
            self.waited += 1;
        } else {
            self.waited = 0;
        }
    }
}

#[modport]
struct BusWatch {
    #[input]
    valid: bool,
    #[input]
    ready: bool,
}

/// A property over a modport.
#[assertions]
#[derive(Default)]
struct BusAssertions {
    #[clock]
    clk: Clock,
    #[modport]
    bus: BusWatch,
}

#[assertions]
impl BusAssertions {
    #[assert]
    fn ready_only_when_valid(&self) {
        if bus.ready {
            bus.valid;
        }
    }
}

verald_host::component! {
    struct Comp { assertions: Assertions<blocks![ReqAssertions, BusAssertions]> }

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

    fn bind_to_module(
        &mut self,
        _ctx: &mut ::veryl_component::SimCtx,
        module: String,
        block: String,
    ) -> ::veryl_component::Result<()> {
        verald_host::entry::register(self.assertions.bind_to_module(module, block))
    }
}

fn str(s: &str) -> Value {
    Value::Str(s.to_string())
}

fn bit(b: bool) -> Value {
    Value::from_u64(u64::from(b), 1)
}

/// A simulator with `clk`, `rst`, and `req`, `ack` and `data` registered
/// under each of `prefixes` (`""` for the root).
fn setup(prefixes: &[&str]) -> (MockSim, Comp) {
    let mut sim = MockSim::new();
    let mut comp = sim.build::<Comp>().unwrap();
    sim.init(&mut comp).unwrap();
    let clk = sim.clock_ref("clk");
    sim.call(&mut comp, "set_clock", &[str("clk"), clk])
        .unwrap();
    let rst = sim.reset_ref("rst");
    sim.call(&mut comp, "set_reset", &[str("rst"), rst])
        .unwrap();
    for prefix in prefixes {
        let at = |m: &str| {
            if prefix.is_empty() {
                m.to_string()
            } else {
                format!("{prefix}.{m}")
            }
        };
        for (name, width) in [("req", 1), ("ack", 1), ("data", 8)] {
            let sig = sim.signal_ref(&at(name), false, &[width]);
            sim.call(&mut comp, "set_input", &[str(&at(name)), sig])
                .unwrap();
        }
        if !prefix.is_empty() {
            // The instance's own clock and reset.
            let clk = sim.clock_ref(&at("clk"));
            sim.call(&mut comp, "set_clock", &[str(&at("clk")), clk])
                .unwrap();
            let rst = sim.reset_ref(&at("rst"));
            sim.call(&mut comp, "set_reset", &[str(&at("rst")), rst])
                .unwrap();
        }
    }
    (sim, comp)
}

fn bind(sim: &mut MockSim, comp: &mut Comp, path: &str, block: &str) {
    sim.call(comp, "bind_to_instance", &[str(path), str(block)])
        .unwrap();
}

fn clock(sim: &mut MockSim, comp: &mut Comp) {
    sim.set_fired_clock("clk");
    sim.clock(comp).unwrap();
}

fn set(sim: &mut MockSim, name: &str, value: Value) {
    sim.set(name, value);
}

#[test]
fn properties_hold_over_a_handshake() {
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    set(&mut sim, "req", bit(true));
    clock(&mut sim, &mut comp);
    set(&mut sim, "req", bit(false));
    set(&mut sim, "ack", bit(true));
    clock(&mut sim, &mut comp);
    set(&mut sim, "ack", bit(false));
    clock(&mut sim, &mut comp);
    assert!(!sim.failed(), "{:?}", sim.failures());
}

#[test]
fn a_failed_property_names_the_expectation() {
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    // `ack` with no request before it.
    set(&mut sim, "ack", bit(true));
    clock(&mut sim, &mut comp);
    assert_eq!(sim.failures().len(), 1, "{:?}", sim.failures());
    assert!(
        sim.failures()[0].contains("assertion `ReqAssertions.ack_answers_a_request` failed at"),
        "{:?}",
        sim.failures()
    );
    assert!(
        sim.failures()[0].ends_with("`self.pending` (verald-assertion/tests/assertions.rs:38)"),
        "{:?}",
        sim.failures()
    );
}

#[test]
fn a_let_in_a_property_shadows_nothing() {
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    set(&mut sim, "data", Value::from_u64(250, 8));
    clock(&mut sim, &mut comp);
    assert_eq!(sim.failures().len(), 1, "{:?}", sim.failures());
    assert!(
        sim.failures()[0].ends_with("`data < limit` (verald-assertion/tests/assertions.rs:45)"),
        "{:?}",
        sim.failures()
    );
}

#[test]
fn a_check_in_a_hook_fails_with_its_message() {
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    set(&mut sim, "req", bit(true));
    for _ in 0..4 {
        clock(&mut sim, &mut comp);
    }
    assert_eq!(sim.failures().len(), 1, "{:?}", sim.failures());
    assert!(
        sim.failures()[0].contains("check in `ReqAssertions` failed at")
            && sim.failures()[0].ends_with(
                "waited 3 edges (`self.waited < 3`) (verald-assertion/tests/assertions.rs:50)"
            ),
        "{:?}",
        sim.failures()
    );
}

#[test]
fn a_reset_disables_the_block_and_clears_its_state() {
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    set(&mut sim, "req", bit(true));
    clock(&mut sim, &mut comp);
    // In reset: nothing is evaluated, and the request seen is forgotten.
    set(&mut sim, "req", bit(false));
    set(&mut sim, "data", Value::from_u64(250, 8));
    sim.set_fired_clock("rst");
    sim.reset(&mut comp).unwrap();
    assert!(!sim.failed(), "{:?}", sim.failures());
    // Out of reset, an `ack` no longer has the request before the reset.
    set(&mut sim, "data", Value::from_u64(0, 8));
    set(&mut sim, "ack", bit(true));
    clock(&mut sim, &mut comp);
    assert_eq!(sim.failures().len(), 1, "{:?}", sim.failures());
    assert!(sim.failures()[0].contains("ack_answers_a_request"));
}

#[test]
fn a_modport_member_is_read_through_its_field() {
    let (mut sim, mut comp) = setup(&[]);
    let bus = sim.modport_ref(
        "bus",
        "bus_if",
        "monitor",
        &[
            ("valid", RefDir::Input, &[1]),
            ("ready", RefDir::Input, &[1]),
        ],
    );
    sim.call(&mut comp, "set_modport", &[str("bus"), bus])
        .unwrap();
    bind(&mut sim, &mut comp, "", "BusAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    set(&mut sim, "bus.ready", bit(true));
    clock(&mut sim, &mut comp);
    assert_eq!(sim.failures().len(), 1, "{:?}", sim.failures());
    assert!(
        sim.failures()[0].contains("`BusAssertions.ready_only_when_valid`")
            && sim.failures()[0].ends_with("`bus.valid` (verald-assertion/tests/assertions.rs:83)"),
        "{:?}",
        sim.failures()
    );
}

#[test]
fn a_wildcard_binds_every_instance_it_matches() {
    let (mut sim, mut comp) = setup(&["u0", "u1"]);
    bind(&mut sim, &mut comp, "u*", "ReqAssertions");
    sim.call(&mut comp, "start", &[]).unwrap();
    set(&mut sim, "u0.ack", bit(true));
    set(&mut sim, "u1.ack", bit(true));
    // Only `u1`'s clock fires: `u0` is not evaluated.
    sim.set_fired_clock("u1.clk");
    sim.clock(&mut comp).unwrap();
    assert_eq!(sim.failures().len(), 1, "{:?}", sim.failures());
    assert!(
        sim.failures()[0].contains("`ReqAssertions@u1.ack_answers_a_request`"),
        "{:?}",
        sim.failures()
    );
}

#[test]
fn binding_errors_are_reported() {
    // An unknown block, at once.
    let (mut sim, mut comp) = setup(&[""]);
    let error = sim
        .call(
            &mut comp,
            "bind_to_instance",
            &[str(""), str("NoAssertions")],
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no assertion block `NoAssertions`; known: ReqAssertions, BusAssertions"),
        "{error}"
    );
    // A module's instances: not yet.
    let error = sim
        .call(
            &mut comp,
            "bind_to_module",
            &[str("Fifo"), str("ReqAssertions")],
        )
        .unwrap_err();
    assert!(error.to_string().contains("not supported yet"), "{error}");
    // A member not registered, at `start`.
    bind(&mut sim, &mut comp, "", "BusAssertions");
    let error = sim.call(&mut comp, "start", &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("binding `BusAssertions`: no modport is registered as `bus`"),
        "{error}"
    );
    // A wildcard matching nothing.
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "dut.*", "ReqAssertions");
    let error = sim.call(&mut comp, "start", &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("binding `ReqAssertions`: no instance matches `dut.*`"),
        "{error}"
    );
    // The same block twice at one instance.
    let (mut sim, mut comp) = setup(&[""]);
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    bind(&mut sim, &mut comp, "", "ReqAssertions");
    let error = sim.call(&mut comp, "start", &[]).unwrap_err();
    assert!(
        error.to_string().contains("`ReqAssertions` is bound twice"),
        "{error}"
    );
    // After `start`.
    let (mut sim, mut comp) = setup(&[""]);
    sim.call(&mut comp, "start", &[]).unwrap();
    let error = sim
        .call(
            &mut comp,
            "bind_to_instance",
            &[str(""), str("ReqAssertions")],
        )
        .unwrap_err();
    assert!(error.to_string().contains("before `start`"), "{error}");
}

/// A list of block types has no limit on its length (one type repeated
/// stands for many here).
#[test]
fn a_list_of_blocks_has_no_limit() {
    type Many = blocks![
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
        BusAssertions,
        ReqAssertions,
    ];
    assert_eq!(<Many as verald_assertion::BlockList>::names().len(), 15);
    let _ = Assertions::<Many>::default();
}
