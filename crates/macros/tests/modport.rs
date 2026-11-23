//! `#[modport]` without the testbench: a struct of inputs only binds and
//! reads through the host layer alone, as an assertion block's modports
//! will.

use verald_host::{InputOnly, ModportRead};
use verald_macros::modport;

#[modport]
struct BusMonitor {
    #[input]
    valid: bool,
    #[input]
    data: u32,
}

#[test]
fn a_modport_of_inputs_needs_no_testbench() {
    fn input_only<T: InputOnly + ModportRead>() {}
    input_only::<BusMonitor>();
}
