# TODO

What is left to do, gathered from the design documents. Each item is
described, with its reasons, where it links to; this list only collects
them.

## Contents

- [Framework](#framework)
- [Parts for VIPs](#parts-for-vips)
- [Host layer](#host-layer)
- [Constrained random](#constrained-random)
- [Assertions](#assertions)
- [Configuration file](#configuration-file)
- [Veryl](#veryl)

## Framework

From [design.md](design.md#todo) and [Taken from UVM](design.md#taken-from-uvm).

- Drain time: a wait between the test's `run` returning and the end of
  the run phase (UVM's `set_drain_time`).
- Ports put in by the attribute (a driver's `#[seq_item_port]`, a
  monitor's `#[analysis_port]`), once Rust's `default_field_values` is
  stable.
- Utility macros for objects (UVM's `uvm_object_utils`, `uvm_field_*`):
  per-field policies, a fixed printout, recording.
- `` `uvm_do ``-like macros (`start_item!`, `finish_item!`, `send_item!`).
- The register model (RAL), a crate of its own (`verald-ral`); models
  could come from rggen.
- `uvm_barrier`; TLM1 put / get / peek ports and `uvm_tlm_fifo`;
  `grab` / `lock`; the response handler; `uvm_sequence_library`;
  comparators as scoreboard templates; severity actions; transaction
  recording; end_of_elaboration and the other phases.

## Parts for VIPs

From [design.md](design.md#for-the-vips): a common library for VIPs (as
tvip's), not the framework.

- An ID manager (`tue_id_manager`).
- A sequence item dispatcher (`tue_sequence_item_dispatcher`).
- An address allocator (`tue_address_allocator`).

## Host layer

From [host-layer-design.md](host-layer-design.md#deferred-items).

- Thread-local access on the per-edge path: to be measured, then reduced.
- Probes and enumerating instances (a change to Veryl).
- Staging only the inputs bound to the event that fired (a change to
  Veryl).
- Traces and other services as layers need them.
- Move the signal fields of clocking blocks and assertion blocks onto the
  component SDK's `Packed` and typed signals (veryl-lang/veryl#3772;
  modports are the SDK's already), keeping the per-edge cost.
- Four-state signals: a field type keeping X and Z, checks for unknown
  values, driving X or Z.
- A two-state bit vector with its width, in place of `BigUint`.
- Not checked yet: the hooks when the design drives a reset (a derived
  reset), and so the tracked reset's state
  ([The state of a reset](host-layer-design.md#the-state-of-a-reset)).

## Constrained random

From [random-design.md](random-design.md#deferred-items).

- `randc`.
- Multi-dimensional arrays (fixed-size only, if ever).
- Signed values wider than 128 bits.
- `randsequence`.
- Caching the IR per type, reusing the CNF across calls.
- The domain analysis stage, splitting independent `Vec` elements.
- Generic `#[randomize]` structs; a random index into an array of
  structs.
- A `sequential` mode for very large arrays.

## Assertions

From [assertion-design.md](assertion-design.md#deferred-items).

- `#[cover]`, with functional and code coverage (per-test results,
  merging, reports).
- Assertions on module instances (`bind_to_module`), with Veryl's
  metadata and probes.
- Reusing assertion blocks: generic blocks and modport structs, for
  interfaces of other widths.
- Named sequences and properties across impls and crates.
- A limit on failures (`max_errors`).
- Structs generated from metadata.
- Emitting SVA from the property IR.
- Formal verification.

## Configuration file

- `Verald.toml` ([config-design.md](config-design.md)): designed, not
  implemented.
- The testbench's code asking for a design's signal or interface by its
  path, in place of registered names, once Veryl gives references by path
  ([`connections` as registrations](config-design.md#connections-as-registrations-for-now)).

## Veryl

- Proposals to Veryl ([veryl-proposals.md](veryl-proposals.md)): clocking
  blocks as a language feature (a study), a hook on reset release, `%m` as
  a format specifier (veryl-lang/veryl#3713).
- After veryl-lang/veryl#3767 is merged: the DPI host's `port_event` and
  port descriptions without the event port, `host::fired` on `ctx.fired`.
- rustdv-veryl's open issues (#1 to #15), for what is brought over from it.
