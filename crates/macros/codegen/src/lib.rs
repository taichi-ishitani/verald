//! Code generation for Verald's signal structs, as functions: `#[modport]`
//! (expanded by `verald-macros`) and the parts `#[clocking]` shares with it
//! (used by `verald-tb-macros`). A library rather than a proc macro crate,
//! so that both macro crates call it; a candidate for moving to Veryl's
//! component SDK, where structs for interfaces may be generated from
//! Veryl's description (veryl-lang/veryl#3712).

pub mod fields;
pub mod modport;
pub mod paths;
