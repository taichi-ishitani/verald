# Configuration file: design

> **Status:** a design, not implemented. It records what has been decided
> about `Verald.toml`, the project's configuration file, and the test
> module's methods that do the same as parts of it, so that the
> implementation and later changes can refer back to it. Parts marked
> *reserved* are in the schema but wait for changes to Veryl; until then a
> file using them is rejected with an error saying so. The file comes after
> the assertions' proof of concept, which binds through the test module's
> methods only (`bind_to_instance`).

## Contents

- [Goals and constraints](#goals-and-constraints)
- [The file](#the-file)
- [Instance paths](#instance-paths)
- [Schema](#schema)
- [How settings are applied](#how-settings-are-applied)
- [The test module's methods](#the-test-modules-methods)
- [Reserved parts and changes to Veryl](#reserved-parts-and-changes-to-veryl)
- [Decisions and rejected alternatives](#decisions-and-rejected-alternatives)

## Goals and constraints

- **Where things bind is not written where they are defined.** An
  assertion block declares what it reads (its members, its clock and
  reset by its own names); where it is bound, and to which signals, is
  given by the configuration or the test module. The same block is bound
  in several places without changing it.
- **One place for what is not code**: the defaults of runtime arguments,
  the assertions bound, and later the signals registered, in one file
  beside `Veryl.toml`.
- **Everything the file does can also be done from the test module**
  where it makes sense, so that a small test needs no file.
- **Mistakes are errors, not ignored**: unknown keys, names bound twice,
  and reserved parts not implemented yet fail the start.
- Read once, at the start of the test: it costs nothing per edge.
- **One Verald component per run.** The file is read by that component,
  so it holds no settings for selecting a component; a component needs
  no instance path of its own.

## The file

- **The default file** is `Verald.toml` at the project's root (the
  directory of `Veryl.toml`). A missing default file is not an error:
  nothing is configured.
- **Another file** is given by the test module (`tb.load_config(path)`) or
  by the runtime argument `VERALD_CONFIG:<path>`, the runtime argument
  winning. The file given replaces the default one; they are not merged.
  A file given and missing is an error.
- **Paths are relative to the project's root.** The file is opened through
  the host (veryl-component's `SimCtx::open`), not `std::fs`: in
  `veryl test` the host resolves relative reads against the project's
  path (`component_file_base`), whatever the working directory, and the
  same call works over the wasm transport. Without a base (unit tests on
  `MockSim`) it falls back to the working directory.
- **When it is read**: at the test module's `start`, when the
  registrations are complete and `load_config` can no longer change. The
  host API opens files only from a `SimCtx`, not at `create`.
- **Format**: TOML, parsed with `toml` and `serde` in the host layer, so
  that a component without the testbench reads it too.

## Instance paths

The names registered with `set_clock`, `set_reset`, `set_modport`,
`set_input` and `set_output` are read as a hierarchy: a registered name
`a.b.c` is the member `c` of the instance `a.b`.

```veryl
tb.set_clock("dut.i_clk", clk);
tb.set_reset("dut.i_rst", rst);
tb.set_modport("dut.bus", bus.monitor);
```

Here `dut` is an instance with the members `i_clk`, `i_rst` and `bus`.
Binding a block to an instance path `p` binds each of its members to the
registered name `p.<member>`, and an empty path binds them to the member
names themselves. A `#[modport]` member binds to the interface registered
at `p.<member>`, and its fields to that interface's members by name.

The hierarchy is only of names for now: the component cannot tell which
design instance a registered signal belongs to (the host API gives it
references, not where they come from). Once Veryl gives probes, the same
paths are resolved in the design instead (see
[Reserved parts](#reserved-parts-and-changes-to-veryl)); registrations
named after the design's hierarchy then keep meaning the same.

**Wildcards** in an instance path: `*` matches one level, `**` any number
of levels (none included). A path with wildcards stands for every
instance it matches among the registered names; a path matching no
instance is an error, as a misspelt path would be.

## Schema

```toml
# Defaults of runtime arguments, used where --runtime-arg does not give
# them. Keys are the arguments' names as given on the command line, so a
# test's own arguments (without the VERALD_ prefix) have defaults too. A
# flag is written `true`. Global: not allowed under [[instances]].
[runtime_args]
VERALD_TIMEOUT = 100000
ROUNDS = 20

# Settings for the instances whose path matches, wildcards allowed.
[[instances]]
path = "dut"
assertions = [
  { block = "FifoAssertions" },                               # bound by member names
  { block = "FifoAssertions", name = "fifo_assertions2", map = { i_clk = "clk2" } },
]
connections = { i_clk = "clk", bus = "bus.monitor" }   # reserved (probes)

[[instances]]
path = "**.u_fifo"
assertions = [{ block = "FifoAssertions" }]

# Defaults for every instance of a module (reserved: Veryl's metadata).
[[modules]]
name = "Fifo"
assertions = [{ block = "FifoAssertions" }]
connections = { i_clk = "i_clk" }                      # reserved (probes)
```

- **`assertions`**: the blocks bound at the instance.
  - `block`: the block's type name, among those the component exports.
  - `name`: the bind's name in reports; the block's name by default.
  - `map`: a block member to the name it binds to instead, relative to the
    instance path (`i_clk = "clk2"` binds `i_clk` to `<path>.clk2`).
    Members not in `map` bind by their own names. A key that is not a
    member of the block is an error.
- **`connections`** (reserved): registrations made by the file, a member
  of the instance to the signal it reads, by its path in the design
  relative to the module the component is instantiated in. The same as
  registering `<path>.<member>` from the test module.
- **Unknown keys** anywhere are errors.

## How settings are applied

For each instance:

1. the matching `[[modules]]`, in the order written;
2. then the matching `[[instances]]`, in the order written.

Within this order:

- **`connections`** merge by member name, the last one winning: a module's
  default is overridden by an instance's.
- **`assertions`** are all taken: the binds from the file and from the
  test module's methods together. Two binds of the same `name` at the
  same instance are an error, found when they are bound (a bind with a
  wildcard is checked at each instance it matches). Binding one block
  twice at one instance therefore needs a `name` for one of them; at two
  instances it does not.
  Binds of the same block to the same signals under different names are
  not checked: evaluated twice, they cost run time only.
- **`runtime_args`**: the command line wins over the test module
  (`tb.option`), which wins over the file's default.

Reports name a bind by its `name` and its instance path
(`FifoAssertions@dut`), since one bind with a wildcard or a module covers
several instances.

## The test module's methods

The component's methods, beside the registrations:

| Method | Does |
|---|---|
| `load_config(path)` | reads `path` in place of the default file |
| `bind_to_instance(path, block)` | binds `block` at the instance `path`, wildcards allowed, by member names, named `block` |
| `bind_to_module(module, block)` | binds `block` at every instance of `module` (reserved: Veryl's metadata) |

A bind needing a `map` or another name is written in the file. The
methods take strings, since Veryl does not pass instances by reference;
they are separate methods, since Veryl's methods have no overloading or
optional arguments. As in `set_*`, where comes first, then what: the
instance or module, then the block.

## Reserved parts and changes to Veryl

| Part | Waits for |
|---|---|
| `connections` | **probes**: a component's input whose source is a signal named by its path in the design, declared before the simulator optimizes; and reading the file at `create`, where probes are declared (the host API opens files only from a `SimCtx`) |
| `[[modules]]`, `bind_to_module` | **metadata** from Veryl: the module each instance is of, with its ports and signals (see [assertion-design.md](assertion-design.md#changes-to-veryl)) |
| instance paths resolved in the design | probes, and enumerating the instances of the design |

Declaring an instance's module by hand (`tb.set_module("dut", "Fifo")`, or
`module = ..` under `[[instances]]`) was considered to have modules
before the metadata, and dropped: the metadata is to come from Veryl.

## Decisions and rejected alternatives

### A file, not strings passed to the test module's methods

Considered: the map passed as a string
(`bind_assertions("BusAssertions", "bus=bus1, clk=clk2")`), and a map built by
several calls (`map_assertions(..)`). Either needs a format or calls of
its own; TOML gives a format with its tooling, and the file holds the
other settings too. The methods are kept for binds by member names.

### Not in `export!`

Rejected: the binds in `export!` (`assertions = [BusAssertions("bus")]`). It ties
where blocks bind to the build of the component, so changing it means
rebuilding, and one component could not be bound differently by two test
modules. `export!` lists the blocks a component has; where they bind is
configuration.

### Registered names as the hierarchy

Considered: a binding API on the component's registrations
(`bind_assertions(block)`) beside binds at instances. Reading the
registered names as instance paths makes it `bind_to_instance("",
block)`, and keeps one meaning when probes resolve the same paths in
the design.

### `runtime_args` global

Runtime arguments are the run's, not an instance's, so they are not
allowed under `[[instances]]`.

### No check of binds to the same signals

Considered: rejecting two binds of a block that resolve to the same
signals under different names. They only cost run time, and the check
would compare every bind's signals; names are checked instead.

### One file replacing the default, not merged

A file given by the test module or the command line replaces the default
one, so that what applies is read from one file.

### One component per run

A run has one Verald component, holding the testbench, the assertions or
both. Selecting settings by component would need the component's instance
path from Veryl (`BuildCtx` has the seed derived from it, not the path),
and nothing needs two components.
