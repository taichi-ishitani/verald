//! The test module's registrations: names given to clocks, resets,
//! interfaces and signals (`set_clock`, `set_reset`, `set_modport`,
//! `set_input`, `set_output`), closed by `start`. Kept by the layer so that
//! every subscriber binds through them, with or without the testbench.

use std::cell::RefCell;

use veryl_component::{ClockPort, InputPort, MemberPort, ModportRef, OutputPort, ResetPort};

use super::{HostError, Signals};

enum Entry {
    Clock(ClockPort),
    Reset(ResetPort),
    Modport(ModportRef),
    Input(InputPort),
    Output(OutputPort),
}

struct Registry {
    entries: Vec<(String, Entry)>,
    started: bool,
}

thread_local! {
    /// The registrations of the simulation running on this thread; `None`
    /// before `on_init`.
    static REGISTRY: RefCell<Option<Registry>> = const { RefCell::new(None) };
}

/// Starts with no registration (`on_init`).
pub(crate) fn init() {
    REGISTRY.with(|r| {
        *r.borrow_mut() = Some(Registry {
            entries: Vec::new(),
            started: false,
        })
    });
}

fn with<R>(f: impl FnOnce(&Registry) -> R) -> Option<R> {
    REGISTRY.with(|r| r.borrow().as_ref().map(f))
}

/// Checks that registrations are still open: after `on_init`, before
/// `start`.
fn check_open(what: &str) -> Result<(), HostError> {
    match with(|r| r.started) {
        None => Err(HostError::new(format!(
            "the component got `{what}` before `on_init`"
        ))),
        Some(true) => Err(HostError::new(format!(
            "`{what}` is only allowed before `start`"
        ))),
        Some(false) => Ok(()),
    }
}

/// Closes the registrations: the test module's `start`. An error if they
/// are closed already, or the simulation has not begun.
pub(crate) fn close() -> Result<(), HostError> {
    check_open("start")?;
    REGISTRY.with(|r| {
        if let Some(registry) = r.borrow_mut().as_mut() {
            registry.started = true;
        }
    });
    Ok(())
}

/// Whether the test module has called `start`.
pub fn started() -> bool {
    with(|r| r.started).unwrap_or(false)
}

fn check_name(name: &str) -> Result<(), HostError> {
    if name.is_empty() {
        return Err(HostError::new("an empty name cannot be registered"));
    }
    if with(|r| r.entries.iter().any(|(n, _)| n == name)).unwrap_or(false) {
        return Err(HostError::new(format!("`{name}` is already registered")));
    }
    Ok(())
}

fn push(name: String, entry: Entry) {
    REGISTRY.with(|r| {
        if let Some(registry) = r.borrow_mut().as_mut() {
            registry.entries.push((name, entry));
        }
    });
}

/// `set_clock(name, clk)`: registers a clock.
pub fn set_clock(name: String, clk: ClockPort) -> Result<(), HostError> {
    check_open("set_clock")?;
    check_name(&name)?;
    push(name, Entry::Clock(clk));
    Ok(())
}

/// `set_reset(name, rst)`: registers a reset, and tracks it
/// (`reset_asserted`).
pub fn set_reset(name: String, rst: ResetPort) -> Result<(), HostError> {
    check_open("set_reset")?;
    check_name(&name)?;
    super::track_reset(rst);
    push(name, Entry::Reset(rst));
    Ok(())
}

/// `set_modport(name, bus.modport)`: registers a modport of an interface;
/// its clock and reset members are registered as `<name>.<member>`.
pub fn set_modport(name: String, bus: ModportRef) -> Result<(), HostError> {
    check_open("set_modport")?;
    check_name(&name)?;
    let mut members = Vec::new();
    for member in bus.members() {
        let path = format!("{name}.{}", member.name);
        match member.port {
            MemberPort::Clock(port) => {
                check_name(&path)?;
                members.push((path, Entry::Clock(port)));
            }
            MemberPort::Reset(port) => {
                check_name(&path)?;
                members.push((path, Entry::Reset(port)));
            }
            MemberPort::Input(_) | MemberPort::Output(_) => {}
        }
    }
    for (path, entry) in members {
        if let Entry::Reset(port) = entry {
            super::track_reset(port);
        }
        push(path, entry);
    }
    push(name, Entry::Modport(bus));
    Ok(())
}

/// `set_input(name, sig)`: registers a signal read, outside any interface
/// (an interrupt, for example).
pub fn set_input(name: String, sig: InputPort) -> Result<(), HostError> {
    check_open("set_input")?;
    check_name(&name)?;
    push(name, Entry::Input(sig));
    Ok(())
}

/// `set_output(name, sig)`: registers a signal driven, outside any
/// interface.
pub fn set_output(name: String, sig: OutputPort) -> Result<(), HostError> {
    check_open("set_output")?;
    check_name(&name)?;
    push(name, Entry::Output(sig));
    Ok(())
}

/// The instance paths the registered names imply, read as a hierarchy: the
/// root (`""`), every proper prefix of a name (`"dut"` of `"dut.i_clk"`),
/// and every modport's name, whose members are its own. In the order first
/// met.
pub fn instance_paths() -> Vec<String> {
    let mut paths = vec![String::new()];
    let mut add = |path: &str| {
        if !paths.iter().any(|p| p == path) {
            paths.push(path.to_string());
        }
    };
    with(|r| {
        for (name, entry) in &r.entries {
            let mut end = 0;
            while let Some(dot) = name[end..].find('.') {
                end += dot;
                add(&name[..end]);
                end += 1;
            }
            if matches!(entry, Entry::Modport(_)) {
                add(name);
            }
        }
    });
    paths
}

/// The clocks registered, in the order registered (an interface's clock
/// members at the interface's place).
pub fn clocks() -> Vec<(String, ClockPort)> {
    with(|r| {
        r.entries
            .iter()
            .filter_map(|(n, e)| match e {
                Entry::Clock(port) => Some((n.clone(), *port)),
                _ => None,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The resets registered, in the order registered.
pub fn resets() -> Vec<(String, ResetPort)> {
    with(|r| {
        r.entries
            .iter()
            .filter_map(|(n, e)| match e {
                Entry::Reset(port) => Some((n.clone(), *port)),
                _ => None,
            })
            .collect()
    })
    .unwrap_or_default()
}

fn find<R>(name: &str, f: impl Fn(&Entry) -> Option<R>) -> Option<R> {
    with(|r| {
        r.entries
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, e)| f(e))
    })
    .flatten()
}

/// The clock registered as `name` (an interface's clock member as
/// `"bus.clk"`).
pub fn clock(name: &str) -> Result<ClockPort, HostError> {
    find(name, |e| match e {
        Entry::Clock(port) => Some(*port),
        _ => None,
    })
    .ok_or_else(|| HostError::new(format!("no clock is registered as `{name}`")))
}

/// The reset registered as `name` (an interface's reset member as
/// `"bus.rst"`).
pub fn reset(name: &str) -> Result<ResetPort, HostError> {
    find(name, |e| match e {
        Entry::Reset(port) => Some(*port),
        _ => None,
    })
    .ok_or_else(|| HostError::new(format!("no reset is registered as `{name}`")))
}

/// The modport registered as `name`.
pub fn modport(name: &str) -> Result<ModportRef, HostError> {
    find(name, |e| match e {
        Entry::Modport(bus) => Some(bus.clone()),
        _ => None,
    })
    .ok_or_else(|| HostError::new(format!("no modport is registered as `{name}`")))
}

/// The modport and member a dotted path names: `bus.done`.
fn member_path(path: &str) -> Option<(ModportRef, &str)> {
    let (modport, member) = path.rsplit_once('.')?;
    Some((self::modport(modport).ok()?, member))
}

/// The signal read at `path`: one registered with `set_input`, or a member
/// of an interface (`"bus.done"`).
pub fn input(path: &str) -> Result<InputPort, HostError> {
    if let Some(port) = find(path, |e| match e {
        Entry::Input(port) => Some(*port),
        _ => None,
    }) {
        return Ok(port);
    }
    match member_path(path) {
        Some((bus, member)) => Ok(bus.input(member)?),
        None => Err(HostError::new(format!(
            "no input is registered as `{path}`"
        ))),
    }
}

/// The signal driven at `path`: one registered with `set_output`, or a
/// member of an interface (`"bus.start"`).
pub fn output(path: &str) -> Result<OutputPort, HostError> {
    if let Some(port) = find(path, |e| match e {
        Entry::Output(port) => Some(*port),
        _ => None,
    }) {
        return Ok(port);
    }
    match member_path(path) {
        Some((bus, member)) => Ok(bus.output(member)?),
        None => Err(HostError::new(format!(
            "no output is registered as `{path}`"
        ))),
    }
}

/// The test module's registrations as `Signals`: what the layers above
/// bind through.
#[derive(Clone, Copy, Debug, Default)]
pub struct Registered;

impl Signals for Registered {
    type Members = ModportRef;
    fn modport(&self, name: &str) -> Result<ModportRef, HostError> {
        modport(name)
    }
    fn input(&self, path: &str) -> Result<InputPort, HostError> {
        input(path)
    }
    fn output(&self, path: &str) -> Result<OutputPort, HostError> {
        output(path)
    }
    fn clock_port(&self, name: &str) -> Result<ClockPort, HostError> {
        clock(name)
    }
    fn reset_port(&self, name: &str) -> Result<ResetPort, HostError> {
        reset(name)
    }
}
