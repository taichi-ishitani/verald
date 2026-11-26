//! The assertions as a subscriber of the host layer's hooks: binds the
//! blocks the test module asked for at `start`, and evaluates every bound
//! block at each edge of its clock.

use verald_host::{self as host, HookSubscriber, HostError, Registered, registrations};

use crate::{AssertionBlock, AssertionSignals, History, Report};

/// A block bound at an instance: its auxiliary state, its properties'
/// attempts and its signals' samples.
pub struct Instance<B: AssertionBlock> {
    label: String,
    block: B,
    state: B::State,
    signals: History<B::Signals>,
}

impl<B: AssertionBlock> Instance<B> {
    fn bind(path: &str, label: String) -> Result<Self, HostError> {
        // The current samples and the `B::PAST` before them.
        let ring = (0..=B::PAST)
            .map(|_| B::Signals::bind(&Registered, path))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| HostError::new(format!("binding `{label}`: {e}")))?;
        Ok(Instance {
            label,
            block: B::default(),
            state: B::State::default(),
            signals: History::new(ring),
        })
    }

    /// At an edge of the block's clock, out of reset: samples, evaluates
    /// the properties, then runs the hooks. (Edges in reset come through
    /// `on_reset`.)
    #[inline]
    fn on_clock(&mut self) {
        if !host::fired(self.signals.clock()) {
            return;
        }
        self.signals.tick();
        let report = Report::new(&self.label);
        self.block.check(&mut self.state, &self.signals, &report);
        self.block.on_clock(&self.signals, &report);
    }

    /// At an edge in reset (the simulator calls `on_reset` instead of
    /// `on_clock` at each edge while the reset is asserted): if the block's
    /// reset fired, the samples move to this edge, as `past` counts edges
    /// in reset too, its attempts are dropped, its state re-initialized and
    /// its `#[on_reset]` hooks run.
    fn on_reset(&mut self) {
        let Some(reset) = self.signals.reset() else {
            return;
        };
        if !host::fired(reset) {
            return;
        }
        self.signals.tick();
        self.block = B::default();
        self.state = B::State::default();
        let report = Report::new(&self.label);
        self.block.on_reset(&self.signals, &report);
    }
}

impl<B: AssertionBlock> Instance<B> {
    /// At the end of the test.
    fn on_finish(&mut self) {
        let report = Report::new(&self.label);
        self.block.finish(&mut self.state, &report);
    }
}

/// The block types a component holds, as a list nested in pairs ending in
/// `()`: `(BusAssertions, (FifoAssertions, ()))`, which [`blocks!`]
/// writes from `blocks![BusAssertions, FifoAssertions]`. One `Vec` of bound
/// instances per type, dispatched statically, with no limit on the number
/// of types.
///
/// [`blocks!`]: crate::blocks
pub trait BlockList: 'static {
    /// The bound instances, one `Vec` per block type, nested as the list.
    type Bound: Default;
    /// The blocks' names, as `bind_to_instance` takes them.
    fn names() -> Vec<&'static str>;
    /// Binds the block named `block` at `path`, named `label`; `false` if
    /// no block has that name.
    fn bind(
        bound: &mut Self::Bound,
        block: &str,
        path: &str,
        label: String,
    ) -> Result<bool, HostError>;
    fn on_clock(bound: &mut Self::Bound);
    fn on_reset(bound: &mut Self::Bound);
    fn on_finish(bound: &mut Self::Bound);
}

/// The end of the list.
impl BlockList for () {
    type Bound = ();

    fn names() -> Vec<&'static str> {
        Vec::new()
    }

    fn bind(_: &mut (), _: &str, _: &str, _: String) -> Result<bool, HostError> {
        Ok(false)
    }

    fn on_clock(_: &mut ()) {}

    fn on_reset(_: &mut ()) {}

    fn on_finish(_: &mut ()) {}
}

/// A block type, then the rest of the list.
impl<B: AssertionBlock, Rest: BlockList> BlockList for (B, Rest) {
    type Bound = (Vec<Instance<B>>, Rest::Bound);

    fn names() -> Vec<&'static str> {
        let mut names = vec![<B::Signals as AssertionSignals>::NAME];
        names.extend(Rest::names());
        names
    }

    fn bind(
        bound: &mut Self::Bound,
        block: &str,
        path: &str,
        label: String,
    ) -> Result<bool, HostError> {
        if block == <B::Signals as AssertionSignals>::NAME {
            bound.0.push(Instance::<B>::bind(path, label)?);
            return Ok(true);
        }
        Rest::bind(&mut bound.1, block, path, label)
    }

    #[inline]
    fn on_clock(bound: &mut Self::Bound) {
        for instance in &mut bound.0 {
            instance.on_clock();
        }
        Rest::on_clock(&mut bound.1);
    }

    fn on_reset(bound: &mut Self::Bound) {
        for instance in &mut bound.0 {
            instance.on_reset();
        }
        Rest::on_reset(&mut bound.1);
    }

    fn on_finish(bound: &mut Self::Bound) {
        for instance in &mut bound.0 {
            instance.on_finish();
        }
        Rest::on_finish(&mut bound.1);
    }
}

/// A bind the test module asked for, resolved at `start`.
struct Request {
    path: String,
    block: String,
}

/// The assertions as a subscriber of the host layer's hooks, over the
/// block types `L` (`Assertions<blocks![BusAssertions, FifoAssertions]>`).
pub struct Assertions<L: BlockList> {
    requests: Vec<Request>,
    bound: L::Bound,
}

impl<L: BlockList> Default for Assertions<L> {
    fn default() -> Self {
        Assertions {
            requests: Vec::new(),
            bound: L::Bound::default(),
        }
    }
}

impl<L: BlockList> Assertions<L> {
    /// `bind_to_instance(path, block)`: binds `block` at the instance
    /// `path` (wildcards allowed: `*` one level, `**` any), every member by
    /// its name, when the test module starts.
    pub fn bind_to_instance(&mut self, path: String, block: String) -> Result<(), HostError> {
        if registrations::started() {
            return Err(HostError::new(
                "`bind_to_instance` is only allowed before `start`",
            ));
        }
        let names = L::names();
        if !names.contains(&block.as_str()) {
            return Err(HostError::new(format!(
                "no assertion block `{block}`; known: {}",
                names.join(", ")
            )));
        }
        self.requests.push(Request { path, block });
        Ok(())
    }

    /// `bind_to_module(module, block)`: binds `block` at every instance of
    /// `module`. Reserved: it needs Veryl's metadata of instances.
    pub fn bind_to_module(&mut self, module: String, block: String) -> Result<(), HostError> {
        Err(HostError::new(format!(
            "`bind_to_module(\"{module}\", \"{block}\")`: binding at a module's instances \
             is not supported yet; use `bind_to_instance`"
        )))
    }

    /// Binds every request at the instances its path matches.
    fn bind_requests(&mut self) -> Result<(), HostError> {
        let mut labels: Vec<String> = Vec::new();
        for request in std::mem::take(&mut self.requests) {
            let paths = if has_wildcard(&request.path) {
                let paths: Vec<String> = registrations::instance_paths()
                    .into_iter()
                    .filter(|p| path_matches(&request.path, p))
                    .collect();
                if paths.is_empty() {
                    return Err(HostError::new(format!(
                        "binding `{}`: no instance matches `{}`",
                        request.block, request.path
                    )));
                }
                paths
            } else {
                vec![request.path.clone()]
            };
            for path in paths {
                let label = if path.is_empty() {
                    request.block.clone()
                } else {
                    format!("{}@{path}", request.block)
                };
                if labels.contains(&label) {
                    return Err(HostError::new(format!("`{label}` is bound twice")));
                }
                L::bind(&mut self.bound, &request.block, &path, label.clone())?;
                labels.push(label);
            }
        }
        Ok(())
    }
}

impl<L: BlockList> HookSubscriber for Assertions<L> {
    fn on_init(&mut self) -> Result<(), HostError> {
        *self = Self::default();
        Ok(())
    }

    fn on_start(&mut self, _test: Option<&str>) -> Result<(), HostError> {
        self.bind_requests()
    }

    fn on_clock(&mut self) -> Result<(), HostError> {
        L::on_clock(&mut self.bound);
        Ok(())
    }

    fn on_reset(&mut self) -> Result<(), HostError> {
        L::on_reset(&mut self.bound);
        Ok(())
    }

    /// Reports the attempts still running (not on a poisoned component:
    /// its state is unknown), then lets go of everything.
    fn on_finish(&mut self) -> Result<(), HostError> {
        if !host::poisoned() {
            L::on_finish(&mut self.bound);
        }
        *self = Self::default();
        Ok(())
    }
}

fn has_wildcard(path: &str) -> bool {
    path.contains('*')
}

/// Whether the instance path `path` matches `pattern`, both dotted: `**`
/// matches any number of levels (none included), `*` within a level any
/// characters.
fn path_matches(pattern: &str, path: &str) -> bool {
    let split = |s: &str| -> Vec<String> {
        if s.is_empty() {
            Vec::new()
        } else {
            s.split('.').map(str::to_string).collect()
        }
    };
    levels_match(&split(pattern), &split(path))
}

fn levels_match(pattern: &[String], path: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| levels_match(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((level, path_rest)) => {
                level_matches(first, level) && levels_match(rest, path_rest)
            }
            None => false,
        },
    }
}

/// `*` within one level: any characters.
fn level_matches(pattern: &str, level: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == level,
        Some((head, tail)) => {
            let Some(rest) = level.strip_prefix(head) else {
                return false;
            };
            (0..=rest.len())
                .filter(|&i| rest.is_char_boundary(i))
                .any(|i| level_matches(tail, &rest[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::path_matches;

    #[test]
    fn wildcards_match_levels() {
        assert!(path_matches("dut", "dut"));
        assert!(!path_matches("dut", "dut.u_fifo"));
        assert!(path_matches("*", "dut"));
        assert!(!path_matches("*", ""));
        assert!(path_matches("dut.*", "dut.u_fifo"));
        assert!(path_matches("**.u_fifo", "u_fifo"));
        assert!(path_matches("**.u_fifo", "dut.a.u_fifo"));
        assert!(path_matches("**", ""));
        assert!(path_matches("**", "a.b"));
        assert!(path_matches("dut*", "dut0"));
        assert!(path_matches("d*t*", "dut0"));
        assert!(!path_matches("dut*", "fifo"));
    }
}
