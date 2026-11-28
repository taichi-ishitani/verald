//! Code generation shared by Verald's macro crates, as functions: the
//! fields of signal structs (`#[clocking]` in `verald-tb-macros`, assertion
//! blocks in `verald-assertion-macros`) and the paths generated code names
//! crates by. A library rather than a proc macro crate, so that both call
//! it. Modport structs are the component SDK's `#[modport]`.

pub mod fields;
pub mod paths;
