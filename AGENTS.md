# Working on Verald

Verald is a UVM-style verification framework in Rust for Veryl. The design
is recorded in [docs/design.md](docs/design.md); read it before working on the
framework. Implementation proceeds step by step (the runtime's core first);
`examples/` holds Veryl projects run with `veryl +local test`.

## How to work

- Write code comments and documents in English.
- Before changing code, explain the purpose and get the user's approval.
- The user decides where a piece of work ends.
- Record design decisions in `docs/design.md` as they are made.
- Keep [docs/todo.md](docs/todo.md), the list of what is left, in step
  with the design documents' TODO and deferred items: add an item there
  when one is deferred, and remove it when it is done.
- Before relying on when the simulator calls a component's hooks, read
  [docs/host-layer-design.md](docs/host-layer-design.md) ("Hooks", "The
  state of a reset"): in a reset the testbench holds, each edge calls
  `on_reset`, not `on_clock`.
- Design choices favour a light run over convenience, and follow the
  principles at the top of `docs/design.md`.

## Git

- Commit and push only when asked.
- Commit messages are one line.
- Fix a pull request by amending its commit.
- Make a new commit for each piece of work unless told to amend.

## Building and testing

- Test with `cargo test`. Run test, fmt and clippy together when verifying.
- Fetching git dependencies needs `CARGO_NET_GIT_FETCH_WITH_CLI=true`
  (git URLs are rewritten to ssh).
- Unreleased Veryl is run as `veryl +local`; rebuild it with
  `verylup install local` in the veryl repository. Otherwise use
  `veryl +latest`.

## Related repositories

For design reference only; code is not copied from them unless the user asks.

| Repository | For | State |
|---|---|---|
| [taichi-ishitani/rustdv-veryl](https://github.com/taichi-ishitani/rustdv-veryl) | the Veryl simulator backend, clocking blocks (`#[modport]` / `#[clocking]`, `tick!`, wait macros, `drive`) and constrained random (`random`, `docs/random.md`) to bring over | open items are its issues #1 to #15 |
| [taichi-ishitani/tue](https://github.com/taichi-ishitani/tue) | the UVM extensions taken in (see "Extensions from tue" in the design) | |
| [taichi-ishitani/tvip-axi](https://github.com/taichi-ishitani/tvip-axi) | an AXI VIP's structure | |
| [taichi-ishitani/tvip-common](https://github.com/taichi-ishitani/tvip-common) | common parts for VIPs | |
| [rggen/rggen-sv-ral](https://github.com/rggen/rggen-sv-ral) | a register model | |
| [rggen/rggen-sample-testbench](https://github.com/rggen/rggen-sample-testbench) | a testbench using the register model | |
| [accellera-official/uvm-core](https://github.com/accellera-official/uvm-core) | UVM's behaviour | |
| [veryl-lang/veryl](https://github.com/veryl-lang/veryl) | veryl-component (`crates/component`) and the simulator (`crates/simulator`) | PRs #3701 (references passed to component methods), #3717 (runtime arguments, `--runtime-arg=NAME[=VALUE]`) and #3719 (`clk.start()`) are merged. PR #3772 (branch `feat/export-rust` of the fork, for issue #3712): packages and interfaces marked `#[export(rust)]` generate Rust types (`import!`), the SDK gains `Packed`, typed signals, `Modport` and `#[modport]` (ported from Verald), and a `[[components]]` entry may be a library (`library = true`, receiving the description without being built as a component; a path dependency's is written too); Verald and its examples depend on that branch until it is merged, then on master. `veryl +local` is built from the same commit as the lock's `veryl-component` (the component's ABI follows Veryl's). The documentation of #3701 is pushed to the veryl-doc fork (branch `component_method_ref_args`), no PR yet. PR #3767 (dalance's, open): references bound as deferred connections; what Verald does after it is in `WORK_NOTES_veryl_pr3767.md` (local) and `docs/todo.md`. The fork's branch `tbf` is no longer used |
| [rustdv/rustdv](https://github.com/rustdv/rustdv) | where rustdv-veryl's backend plugs in | PR #25 (branch `gpi-backend-trait` of the fork): a public `Backend` trait. An empty local branch `feat/take-in-place-children` remains; whether to delete it is undecided |

tue and tvip may be named in this repository's documents.
