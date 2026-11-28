//! A VIP for the register block's valid/ready bus. It provides what a
//! project using the bus needs:
//!
//! - the interface, for Veryl (`bus_if.veryl`, marked `#[export(rust)]`),
//!   its modports generated here ([`bus_if`]);
//! - the testbench's agent ([`agent`]);
//! - the bus's assertions ([`assertions`]).

#![allow(async_fn_in_trait)]

// The bus's interface is the VIP's own: its modports are generated from the
// description Veryl writes into this crate at each run (`veryl.types.json`):
// `bus_if::Master` for `master`, `bus_if::Monitor` for `monitor`.
veryl_component::import!(bus_if);

pub mod agent;
pub mod assertions;
