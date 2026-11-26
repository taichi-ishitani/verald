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
| [veryl-lang/veryl](https://github.com/veryl-lang/veryl) | veryl-component (`crates/component`) and the simulator (`crates/simulator`) | PR #3701 (branch `feat/component-method-ref-args` of the fork): signal and modport references passed to component methods, clocks and resets in interfaces, reset aliases, ports shared per event. Related issues #3699, #3698 (`clk.run()`). Its documentation is pushed to the veryl-doc fork (branch `component_method_ref_args`), no PR yet. PR #3717 (branch `runtime_args`): runtime arguments (`--runtime-arg`), from issue #3716. PR #3719 (branch `feature/clock-gen-start`): `clk.start()` on `$tb::clock_gen`, a clock running until the test ends, from issue #3698. The fork's branch `tbf` merges the PRs Verald needs; Verald and its examples depend on it |
| [rustdv/rustdv](https://github.com/rustdv/rustdv) | where rustdv-veryl's backend plugs in | PR #25 (branch `gpi-backend-trait` of the fork): a public `Backend` trait. An empty local branch `feat/take-in-place-children` remains; whether to delete it is undecided |

tue and tvip may be named in this repository's documents.
