//! A DPI host for Verald testbenches (a prototype): runs a testbench
//! component on a SystemVerilog simulator, through DPI functions a
//! SystemVerilog wrapper calls. See `docs/sv-simulators.md`.
//!
//! The component is hosted as Veryl's simulator hosts it, through the SDK's
//! C ABI (`veryl_component::export` and `sys::VrlHostApi`), so the
//! testbench's code is unchanged. The wrapper:
//!
//! - loads a table describing how the signals bound to the testbench (a
//!   modport, or a single signal: a `signal_map` entry each) are packed (`verald_init`), and binds the names the
//!   testbench looks up to the table's keys (`verald_set_clock`, ..);
//! - at each edge, hands each entry's inputs over (`verald_sample`), calls
//!   the hook (`verald_on_clock` / `verald_on_reset`), and takes each entry's
//!   outputs back, all of them when any was written (`verald_drive`);
//! - polls the test's status (`verald_status`) and ends the simulation.
//!
//! An entry's inputs, and its outputs, are packed in the table's order,
//! the first member most significant, as SystemVerilog's `{a, b, c}`; the
//! vectors cross DPI as `bit [VERALD_MAX_W-1:0]`, zero-extended (a prototype's
//! simplification of open arrays), whose width the wrapper gives
//! `verald_init`.
//!
//! `export!(Component)` in the testbench's crate generates the DPI
//! functions; `crates/tb/dpi/verald_dpi_pkg.sv` declares them for the wrapper.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::Deserialize;
use veryl_component::sys;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Table {
    #[serde(default)]
    layout: HashMap<String, Layout>,
    #[serde(default)]
    signal_map: Vec<SignalMapEntry>,
    #[serde(default)]
    clock: Vec<ClockEntry>,
    #[serde(default)]
    reset: Vec<ResetEntry>,
}

#[derive(Deserialize)]
struct Layout {
    members: Vec<MemberEntry>,
}

#[derive(Deserialize, Clone)]
struct MemberEntry {
    name: String,
    dir: Dir,
    width: u32,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Dir {
    In,
    Out,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Modport,
    Signal,
}

#[derive(Deserialize)]
struct SignalMapEntry {
    key: i32,
    path: String,
    kind: Kind,
    layout: String,
    #[serde(default)]
    interface: String,
    #[serde(default)]
    modport: String,
}

#[derive(Deserialize)]
struct ClockEntry {
    key: i32,
    path: String,
    reset: Option<i32>,
}

#[derive(Deserialize)]
struct ResetEntry {
    key: i32,
    path: String,
}

// ---------------------------------------------------------------------------
// The host's state
// ---------------------------------------------------------------------------

struct Port {
    name: String,
    output: bool,
    role: Role,
    width: u32,
    words: Vec<u64>,
    mask: Vec<u64>,
    /// Set by the component's writes (the host API's or a direct one).
    dirty: Box<u8>,
    desc: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Data,
    Clock,
    Reset,
}

/// A `signal_map` entry, bound: its ports, inputs and outputs, in the table's
/// order.
struct SignalMap {
    kind: Kind,
    inputs: Vec<u32>,
    outputs: Vec<u32>,
    all: Vec<u32>,
}

/// The test's status, as `verald_status` reports it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Running = 0,
    Passed = 1,
    Failed = 2,
}

struct Host {
    vtable: sys::VrlComponentVTable,
    state: *mut c_void,
    ports: Vec<Port>,
    signal_maps: HashMap<i32, SignalMap>,
    clocks: HashMap<i32, (u32, Option<i32>)>,
    resets: HashMap<i32, u32>,
    time: u64,
    seed: u64,
    /// 32-bit words of the vectors the DPI functions carry
    /// (`VERALD_MAX_W`, which the wrapper gives `verald_init`).
    vector_words: usize,
    fired: u32,
    status: Status,
}

thread_local! {
    /// The host, leaked for the simulation's lifetime: the component calls
    /// back into it (through `VrlCtx`, which is this pointer) while a DPI
    /// function holds it, so it is reached through a raw pointer, never
    /// borrowed across a call into the component.
    static HOST: Cell<*mut Host> = const { Cell::new(std::ptr::null_mut()) };
    /// The instance's label and the edges counted, for messages as Veryl's
    /// simulator writes them (`[tb] cycle 71: ..`).
    static LABEL: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    static CYCLE: Cell<u64> = const { Cell::new(0) };
}

fn host_ptr() -> Option<*mut Host> {
    let p = HOST.with(Cell::get);
    (!p.is_null()).then_some(p)
}

fn ctx(host: *mut Host) -> *mut sys::VrlCtx {
    host as *mut sys::VrlCtx
}

/// # Safety
/// `ctx` is the host pointer this crate passed to the component.
unsafe fn host_of<'a>(ctx: *mut sys::VrlCtx) -> &'a mut Host {
    unsafe { &mut *(ctx as *mut Host) }
}

fn words64(width: u32) -> usize {
    width.div_ceil(64).max(1) as usize
}

/// Prints a message as Veryl's simulator prints a component's:
/// `[<instance>] cycle <n>: <message>`.
fn print(msg: &str) {
    let label = LABEL.with(|l| l.borrow().clone());
    if label.is_empty() {
        println!("{msg}");
    } else {
        println!("[{label}] cycle {}: {msg}", CYCLE.with(Cell::get));
    }
}

// ---------------------------------------------------------------------------
// The host API the component calls
// ---------------------------------------------------------------------------

unsafe extern "C" fn api_port_index(ctx: *mut sys::VrlCtx, name: sys::VrlStr, dir: u32) -> i32 {
    let host = unsafe { host_of(ctx) };
    let name = unsafe { name.as_str() };
    host.ports
        .iter()
        .position(|p| {
            p.name == name
                && match dir {
                    sys::VRL_DIR_OUTPUT => p.output,
                    sys::VRL_DIR_CLOCK => p.role == Role::Clock,
                    sys::VRL_DIR_RESET => p.role == Role::Reset,
                    _ => !p.output,
                }
        })
        .map_or(-1, |i| i as i32)
}

unsafe extern "C" fn api_port_width(ctx: *mut sys::VrlCtx, idx: u32) -> u32 {
    let host = unsafe { host_of(ctx) };
    host.ports.get(idx as usize).map_or(0, |p| p.width)
}

unsafe extern "C" fn api_read_input(
    ctx: *mut sys::VrlCtx,
    idx: u32,
    words: *mut u64,
    mask_xz: *mut u64,
) {
    let host = unsafe { host_of(ctx) };
    let Some(port) = host.ports.get(idx as usize) else {
        return;
    };
    unsafe {
        std::ptr::copy_nonoverlapping(port.words.as_ptr(), words, port.words.len());
        if !mask_xz.is_null() {
            std::ptr::write_bytes(mask_xz, 0, port.words.len());
        }
    }
}

unsafe extern "C" fn api_write_output(
    ctx: *mut sys::VrlCtx,
    idx: u32,
    words: *const u64,
    _mask_xz: *const u64,
) {
    let host = unsafe { host_of(ctx) };
    let Some(port) = host.ports.get_mut(idx as usize) else {
        return;
    };
    if !port.output {
        return;
    }
    unsafe { std::ptr::copy_nonoverlapping(words, port.words.as_mut_ptr(), port.words.len()) };
    *port.dirty = 1;
}

unsafe extern "C" fn api_param_get(
    _ctx: *mut sys::VrlCtx,
    _name: sys::VrlStr,
    _out: *mut sys::VrlValue,
) -> i32 {
    -1
}

unsafe extern "C" fn api_fail(ctx: *mut sys::VrlCtx, msg: sys::VrlStr) {
    let host = unsafe { host_of(ctx) };
    print(unsafe { msg.as_str() });
    host.status = Status::Failed;
}

unsafe extern "C" fn api_finish(ctx: *mut sys::VrlCtx) {
    let host = unsafe { host_of(ctx) };
    if host.status == Status::Running {
        host.status = Status::Passed;
    }
}

unsafe extern "C" fn api_log(_ctx: *mut sys::VrlCtx, msg: sys::VrlStr) {
    print(unsafe { msg.as_str() });
}

unsafe extern "C" fn api_cycle(_ctx: *mut sys::VrlCtx) -> u64 {
    CYCLE.with(Cell::get)
}

unsafe extern "C" fn api_sim_time(ctx: *mut sys::VrlCtx) -> u64 {
    unsafe { host_of(ctx) }.time
}

unsafe extern "C" fn api_seed(ctx: *mut sys::VrlCtx) -> u64 {
    unsafe { host_of(ctx) }.seed
}

unsafe extern "C" fn api_fired_clock(ctx: *mut sys::VrlCtx) -> u32 {
    unsafe { host_of(ctx) }.fired
}

unsafe extern "C" fn api_file_open(_ctx: *mut sys::VrlCtx, _path: sys::VrlStr, _mode: u32) -> i32 {
    -1
}

unsafe extern "C" fn api_file_read(
    _ctx: *mut sys::VrlCtx,
    _h: i32,
    _buf: *mut u8,
    _len: usize,
) -> i64 {
    -1
}

unsafe extern "C" fn api_file_write(
    _ctx: *mut sys::VrlCtx,
    _h: i32,
    _buf: *const u8,
    _len: usize,
) -> i64 {
    -1
}

unsafe extern "C" fn api_file_seek(
    _ctx: *mut sys::VrlCtx,
    _h: i32,
    _pos: i64,
    _whence: u32,
) -> i64 {
    -1
}

unsafe extern "C" fn api_file_close(_ctx: *mut sys::VrlCtx, _h: i32) {}

unsafe extern "C" fn api_trace_var(_ctx: *mut sys::VrlCtx, _name: sys::VrlStr, _width: u32) -> i32 {
    -1
}

unsafe extern "C" fn api_trace_write(_ctx: *mut sys::VrlCtx, _h: i32, _words: *const u64) {}

unsafe extern "C" fn api_is_4state(_ctx: *mut sys::VrlCtx) -> u32 {
    0
}

unsafe extern "C" fn api_port_direct(
    ctx: *mut sys::VrlCtx,
    idx: u32,
    out: *mut sys::VrlPortDirect,
) -> u32 {
    let host = unsafe { host_of(ctx) };
    let Some(port) = host.ports.get_mut(idx as usize) else {
        return 0;
    };
    // The buffers never move: the ports are all made before the component.
    unsafe {
        *out = sys::VrlPortDirect {
            words: port.words.as_mut_ptr(),
            mask_xz: port.mask.as_mut_ptr(),
            dirty: if port.output {
                &mut *port.dirty as *mut u8
            } else {
                std::ptr::null_mut()
            },
        };
    }
    1
}

unsafe extern "C" fn api_port_desc(
    ctx: *mut sys::VrlCtx,
    idx: u32,
    buf: *mut u8,
    cap: usize,
) -> i64 {
    let host = unsafe { host_of(ctx) };
    let Some(port) = host.ports.get(idx as usize) else {
        return -1;
    };
    let bytes = port.desc.as_bytes();
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len().min(cap)) };
    bytes.len() as i64
}

unsafe extern "C" fn api_write_output_bits(
    ctx: *mut sys::VrlCtx,
    idx: u32,
    lsb: u32,
    width: u32,
    words: *const u64,
    _mask_xz: *const u64,
) -> i32 {
    let host = unsafe { host_of(ctx) };
    let Some(port) = host.ports.get_mut(idx as usize) else {
        return -1;
    };
    if !port.output || lsb.checked_add(width).is_none_or(|end| end > port.width) {
        return -1;
    }
    let src = unsafe { std::slice::from_raw_parts(words, words64(width)) };
    for i in 0..width {
        let bit = (src[(i / 64) as usize] >> (i % 64)) & 1;
        let at = lsb + i;
        let word = &mut port.words[(at / 64) as usize];
        *word = (*word & !(1 << (at % 64))) | (bit << (at % 64));
    }
    *port.dirty = 1;
    0
}

// Runtime arguments: none yet. Taking them from the SystemVerilog
// simulator's plusargs is left for later.
unsafe extern "C" fn api_runtime_arg_count(_: *mut sys::VrlCtx) -> u32 {
    0
}

unsafe extern "C" fn api_runtime_arg_text(
    _: *mut sys::VrlCtx,
    _idx: u32,
    _buf: *mut u8,
    _cap: usize,
) -> i64 {
    -1
}

static API: sys::VrlHostApi = sys::VrlHostApi {
    size: size_of::<sys::VrlHostApi>(),
    port_index: api_port_index,
    port_width: api_port_width,
    read_input: api_read_input,
    write_output: api_write_output,
    param_get: api_param_get,
    fail: api_fail,
    finish: api_finish,
    log: api_log,
    cycle: api_cycle,
    sim_time: api_sim_time,
    seed: api_seed,
    fired_clock: api_fired_clock,
    file_open: api_file_open,
    file_read: api_file_read,
    file_write: api_file_write,
    file_seek: api_file_seek,
    file_close: api_file_close,
    trace_var: api_trace_var,
    trace_write: api_trace_write,
    is_4state: api_is_4state,
    port_direct: api_port_direct,
    port_desc: api_port_desc,
    write_output_bits: api_write_output_bits,
    runtime_arg_count: api_runtime_arg_count,
    runtime_arg_name: api_runtime_arg_text,
    runtime_arg_value: api_runtime_arg_text,
};

// ---------------------------------------------------------------------------
// The DPI functions, behind `export!`
// ---------------------------------------------------------------------------

/// Runs `f`, turning a panic into a failure, so none crosses into the
/// simulator.
fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            print("the DPI host panicked");
            if let Some(host) = host_ptr() {
                unsafe { (*host).status = Status::Failed };
            }
            fallback
        }
    }
}

fn c_str<'a>(s: *const c_char) -> &'a str {
    if s.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(s) }.to_str().unwrap_or("")
}

fn add_port(
    ports: &mut Vec<Port>,
    name: String,
    output: bool,
    role: Role,
    width: u32,
    desc: String,
) -> u32 {
    let n = words64(width);
    ports.push(Port {
        name,
        output,
        role,
        width,
        words: vec![0; n],
        mask: vec![0; n],
        dirty: Box::new(0),
        desc,
    });
    (ports.len() - 1) as u32
}

fn desc(fields: [&str; 8]) -> String {
    fields.join(&sys::VRL_PORT_DESC_SEP.to_string())
}

/// `verald_init(label, table, seed, vector_width)`: loads the table, makes the
/// ports, creates the component and calls `on_init`. `label` names the
/// instance in messages; `vector_width` is the width of the vectors the DPI
/// functions carry (`VERALD_MAX_W`). 0 on success.
pub fn init(
    vtable: sys::VrlComponentVTable,
    label: *const c_char,
    table: *const c_char,
    seed: u64,
    vector_width: i32,
) -> i32 {
    guard(1, || {
        LABEL.with(|l| *l.borrow_mut() = c_str(label).to_string());
        let Ok(vector_width) = u32::try_from(vector_width) else {
            print(&format!("the vector width {vector_width} is negative"));
            return 1;
        };
        let path = c_str(table);
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                print(&format!("reading the table `{path}`: {e}"));
                return 1;
            }
        };
        let table: Table = match toml::from_str(&text) {
            Ok(t) => t,
            Err(e) => {
                print(&format!("the table `{path}`: {e}"));
                return 1;
            }
        };
        let mut ports = Vec::new();
        let mut signal_maps = HashMap::new();
        for entry in &table.signal_map {
            let Some(layout) = table.layout.get(&entry.layout) else {
                print(&format!("no layout `{}` in the table", entry.layout));
                return 1;
            };
            let mut signal_map = SignalMap {
                kind: entry.kind,
                inputs: Vec::new(),
                outputs: Vec::new(),
                all: Vec::new(),
            };
            for m in &layout.members {
                let output = m.dir == Dir::Out;
                let dirtext = if output { "output" } else { "input" };
                let width = m.width.to_string();
                let idx = match entry.kind {
                    Kind::Modport => {
                        let name = sys::member_port_name(&entry.path, &m.name);
                        let d = desc([
                            dirtext,
                            &width,
                            &width,
                            "",
                            &name,
                            &m.name,
                            &entry.interface,
                            &entry.modport,
                        ]);
                        add_port(&mut ports, name, output, Role::Data, m.width, d)
                    }
                    Kind::Signal => {
                        let d = desc([dirtext, &width, &width, "", &entry.path, "", "", ""]);
                        add_port(
                            &mut ports,
                            entry.path.clone(),
                            output,
                            Role::Data,
                            m.width,
                            d,
                        )
                    }
                };
                if output {
                    signal_map.outputs.push(idx);
                } else {
                    signal_map.inputs.push(idx);
                }
                signal_map.all.push(idx);
            }
            signal_maps.insert(entry.key, signal_map);
        }
        let mut resets = HashMap::new();
        for r in &table.reset {
            let d = desc(["reset", "1", "1", "", &r.path, "", "", ""]);
            resets.insert(
                r.key,
                add_port(&mut ports, r.path.clone(), false, Role::Reset, 1, d),
            );
        }
        let mut clocks = HashMap::new();
        for c in &table.clock {
            let d = desc(["clock", "1", "1", "", &c.path, "", "", ""]);
            let idx = add_port(&mut ports, c.path.clone(), false, Role::Clock, 1, d);
            clocks.insert(c.key, (idx, c.reset));
        }
        let host = Box::into_raw(Box::new(Host {
            vtable,
            state: std::ptr::null_mut(),
            ports,
            signal_maps,
            clocks,
            resets,
            time: 0,
            seed,
            vector_words: vector_width.div_ceil(32) as usize,
            fired: u32::MAX,
            status: Status::Running,
        }));
        HOST.with(|h| h.set(host));
        let create = unsafe { (*host).vtable.create };
        let state = unsafe { create(ctx(host), &API) };
        if state.is_null() {
            unsafe { (*host).status = Status::Failed };
            return 1;
        }
        unsafe { (*host).state = state };
        let on_init = unsafe { (*host).vtable.on_init };
        unsafe { on_init(state, ctx(host)) }
    })
}

/// An argument for a method call: a string, bits or a reference.
enum Arg {
    Str(String),
    Bits(u64, u32),
    Ref(u32, Vec<u64>),
}

/// Calls a method of the component; 0 on success.
fn call(name: &str, args: Vec<Arg>) -> i32 {
    let Some(host) = host_ptr() else {
        print("`verald_init` was not called");
        return 1;
    };
    let mut words: Vec<Vec<u64>> = Vec::new();
    let values: Vec<sys::VrlValue> = args
        .iter()
        .map(|arg| {
            let mut v = sys::VrlValue::unit();
            match arg {
                Arg::Str(s) => {
                    v.kind = sys::VRL_VALUE_STRING;
                    v.str_ = sys::VrlStr::from_str(s);
                }
                Arg::Bits(value, width) => {
                    words.push(vec![*value]);
                    v.kind = sys::VRL_VALUE_BITS;
                    v.width = *width;
                    v.words = words.last().expect("pushed").as_ptr();
                    v.nwords = 1;
                }
                Arg::Ref(kind, ports) => {
                    v.kind = sys::VRL_VALUE_REF;
                    v.width = *kind;
                    v.words = ports.as_ptr();
                    v.nwords = ports.len();
                }
            }
            v
        })
        .collect();
    let mut ret_words = [0u64; 8];
    let mut ret = sys::VrlValue::unit();
    ret.words = ret_words.as_mut_ptr();
    ret.nwords = ret_words.len();
    let (call_method, state) = unsafe { ((*host).vtable.call_method, (*host).state) };
    unsafe {
        call_method(
            state,
            ctx(host),
            sys::VrlStr::from_str(name),
            values.as_ptr(),
            values.len(),
            &mut ret,
        )
    }
}

fn ref_of(kind: Kind, ports: &[u32]) -> Arg {
    let kind = match kind {
        Kind::Modport => sys::VRL_REF_MODPORT,
        Kind::Signal => sys::VRL_REF_SIGNAL,
    };
    Arg::Ref(kind, ports.iter().map(|p| u64::from(*p)).collect())
}

/// `verald_set_clock` / `verald_set_reset`: binds `name` to a clock or a reset.
pub fn set_clock_or_reset(method: &str, name: *const c_char, key: i32) -> i32 {
    guard(1, || {
        let Some(host) = host_ptr() else { return 1 };
        let port = unsafe {
            if method == "set_clock" {
                (*host).clocks.get(&key).map(|c| c.0)
            } else {
                (*host).resets.get(&key).copied()
            }
        };
        let Some(port) = port else {
            print(&format!("`{method}`: no key {key} in the table"));
            return 1;
        };
        call(
            method,
            vec![
                Arg::Str(c_str(name).to_string()),
                ref_of(Kind::Signal, &[port]),
            ],
        )
    })
}

/// `verald_set_interface` / `verald_set_input` / `verald_set_output`: binds `name`
/// to a `signal_map` entry of the table.
pub fn set_signal_map(method: &str, name: *const c_char, key: i32) -> i32 {
    guard(1, || {
        let Some(host) = host_ptr() else { return 1 };
        let Some(arg) = (unsafe { (*host).signal_maps.get(&key) }).map(|i| ref_of(i.kind, &i.all))
        else {
            print(&format!("`{method}`: no signal_map {key} in the table"));
            return 1;
        };
        call(method, vec![Arg::Str(c_str(name).to_string()), arg])
    })
}

/// `verald_option` / `verald_set`: a 64-bit value under a key.
pub fn set_value(method: &str, key: *const c_char, value: i64) -> i32 {
    guard(1, || {
        call(
            method,
            vec![
                Arg::Str(c_str(key).to_string()),
                Arg::Bits(value as u64, 64),
            ],
        )
    })
}

/// `verald_start(test, time)`: starts the test; 0 if it started.
pub fn start(test: *const c_char, time: u64) -> i32 {
    guard(1, || {
        let Some(host) = host_ptr() else { return 1 };
        unsafe { (*host).time = time };
        let r = call("start", vec![Arg::Str(c_str(test).to_string())]);
        if r != 0 || unsafe { (*host).status } == Status::Failed {
            1
        } else {
            0
        }
    })
}

fn bit(words: &[u32], at: u32) -> u64 {
    u64::from((words[(at / 32) as usize] >> (at % 32)) & 1)
}

/// `verald_sample(key, inputs)`: a `signal_map` entry's inputs, packed in the table's
/// order with the first most significant, before the hook.
///
/// # Safety
/// `inputs` points to the vector width given `verald_init` (the DPI
/// `bit [VERALD_MAX_W-1:0]`).
pub unsafe fn sample(key: i32, inputs: *const u32) {
    guard((), || {
        let Some(host) = host_ptr() else { return };
        let host = unsafe { &mut *host };
        let Some(signal_map) = host.signal_maps.get(&key) else {
            return;
        };
        let words = unsafe { std::slice::from_raw_parts(inputs, host.vector_words) };
        // The last input is at bit 0.
        let mut at = 0;
        for &idx in signal_map.inputs.iter().rev() {
            let port = &mut host.ports[idx as usize];
            port.words.iter_mut().for_each(|w| *w = 0);
            for i in 0..port.width {
                port.words[(i / 64) as usize] |= bit(words, at + i) << (i % 64);
            }
            at += port.width;
        }
    })
}

/// `verald_drive(key, outputs)`: a `signal_map` entry's outputs after the
/// hook, packed as its inputs are; 1 if any of them was written since the
/// last drive, when the wrapper updates them all, 0 otherwise.
///
/// # Safety
/// `outputs` points to the vector width given `verald_init`, writable.
pub unsafe fn drive(key: i32, outputs: *mut u32) -> u8 {
    guard(0, || {
        let Some(host) = host_ptr() else { return 0 };
        let host = unsafe { &mut *host };
        let out = unsafe { std::slice::from_raw_parts_mut(outputs, host.vector_words) };
        out.iter_mut().for_each(|w| *w = 0);
        if host.status != Status::Running {
            return 0;
        }
        let Some(signal_map) = host.signal_maps.get(&key) else {
            return 0;
        };
        let mut written = false;
        let mut at = 0;
        for &idx in signal_map.outputs.iter().rev() {
            let port = &mut host.ports[idx as usize];
            written |= *port.dirty != 0;
            *port.dirty = 0;
            for i in 0..port.width {
                let b = at + i;
                let v = (port.words[(i / 64) as usize] >> (i % 64)) & 1;
                out[(b / 32) as usize] |= (v as u32) << (b % 32);
            }
            at += port.width;
        }
        u8::from(written)
    })
}

/// `verald_on_clock` / `verald_on_reset`: the hook of a clock, or of its reset,
/// at `time`. Nothing once the test has ended.
pub fn hook(reset: bool, clock: i32, time: u64) {
    guard((), || {
        let Some(host) = host_ptr() else { return };
        let (fired, f, state) = unsafe {
            let h = &mut *host;
            if h.status != Status::Running || h.state.is_null() {
                return;
            }
            h.time = time;
            let Some(&(clock_port, reset_key)) = h.clocks.get(&clock) else {
                return;
            };
            let fired = if reset {
                match reset_key.and_then(|r| h.resets.get(&r)) {
                    Some(&p) => p,
                    None => return,
                }
            } else {
                clock_port
            };
            let f = if reset {
                h.vtable.on_reset
            } else {
                h.vtable.on_clock
            };
            (fired, f, h.state)
        };
        CYCLE.with(|c| c.set(c.get() + 1));
        unsafe { (*host).fired = fired };
        unsafe { f(state, ctx(host)) };
    })
}

/// `verald_status()`: 0 running, 1 passed, 2 failed.
pub fn status() -> i32 {
    host_ptr().map_or(2, |h| unsafe { (*h).status } as i32)
}

/// `verald_on_finish(time)`: from `final`; reports a finish the test did not
/// ask for.
pub fn on_finish(time: u64) {
    guard((), || {
        let Some(host) = host_ptr() else { return };
        let (f, state) = unsafe {
            (*host).time = time;
            ((*host).vtable.on_finish, (*host).state)
        };
        if !state.is_null() {
            unsafe { f(state, ctx(host)) };
        }
    })
}

/// Generates the DPI functions for the testbench component `$ty` (the type
/// `verald_tb_runtime::component!` makes). Invoke it once, in place of
/// `export!`, in a build for a SystemVerilog simulator.
#[macro_export]
macro_rules! export {
    ($ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn verald_init(
            label: *const ::std::ffi::c_char,
            table: *const ::std::ffi::c_char,
            seed: u64,
            vector_width: i32,
        ) -> i32 {
            $crate::init(
                ::veryl_component::export::vtable::<$ty>(),
                label,
                table,
                seed,
                vector_width,
            )
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_set_clock(name: *const ::std::ffi::c_char, key: i32) -> i32 {
            $crate::set_clock_or_reset("set_clock", name, key)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_set_reset(name: *const ::std::ffi::c_char, key: i32) -> i32 {
            $crate::set_clock_or_reset("set_reset", name, key)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_set_interface(name: *const ::std::ffi::c_char, key: i32) -> i32 {
            $crate::set_signal_map("set_interface", name, key)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_set_input(name: *const ::std::ffi::c_char, key: i32) -> i32 {
            $crate::set_signal_map("set_input", name, key)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_set_output(name: *const ::std::ffi::c_char, key: i32) -> i32 {
            $crate::set_signal_map("set_output", name, key)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_option(key: *const ::std::ffi::c_char, value: i64) -> i32 {
            $crate::set_value("option", key, value)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_set(key: *const ::std::ffi::c_char, value: i64) -> i32 {
            $crate::set_value("set", key, value)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_start(test: *const ::std::ffi::c_char, time: u64) -> i32 {
            $crate::start(test, time)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_sample(key: i32, inputs: *const u32) {
            // SAFETY: the wrapper passes a `bit [VERALD_MAX_W-1:0]`.
            unsafe { $crate::sample(key, inputs) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_on_clock(clock: i32, time: u64) {
            $crate::hook(false, clock, time)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_on_reset(clock: i32, time: u64) {
            $crate::hook(true, clock, time)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_drive(key: i32, outputs: *mut u32) -> u8 {
            // SAFETY: the wrapper passes a `bit [VERALD_MAX_W-1:0]` output.
            unsafe { $crate::drive(key, outputs) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_status() -> i32 {
            $crate::status()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn verald_on_finish(time: u64) {
            $crate::on_finish(time)
        }
    };
}
