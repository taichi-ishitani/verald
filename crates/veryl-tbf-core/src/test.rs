//! Tests, their specs, and the runner: registration, selection, the run
//! phase's tasks, and the end (`check`, `report`, the result).

use std::rc::Rc;

use veryl_component::Value;
use veryl_tbf_runtime::{RunEnd, Start, TestError, Testbench, yield_now};

use crate::component::{
    CheckContext, Component, Instance, Node, Phase, ReportContext, RunContext, Scope, set_phase,
};
use crate::report;

/// A test: the root of the hierarchy. It is made in two steps, its
/// configuration (`setup`) and then its hierarchy (`Component::build`, from
/// that configuration), with a spec applied after each. It runs in three:
/// `pre_run`, `run` (the scenario) and `post_run`; the run phase ends when
/// `post_run` returns.
pub trait Test: Component {
    /// Makes the configuration the test is built from (may randomize it).
    fn setup(setup_context: &mut SetupContext) -> Result<Self::Context, TestError>;

    /// Before `run`: the preparation every scenario shares (waiting for the
    /// reset, setting the DUT up).
    async fn pre_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        Ok(())
    }

    /// After `run`: the wind-up every scenario shares (waiting for the bus
    /// to go idle, reading the status).
    async fn post_run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        Ok(())
    }
}

/// What a test made from a base test runs for a step, on the base:
/// `#[component(test(base = B, run = S))]`. It plays a virtual sequence,
/// with the test as its virtual sequencer.
pub trait Scenario: Default + 'static {
    /// The test it runs on.
    type Test: Test;

    async fn body(
        &mut self,
        test: &mut Self::Test,
        run_context: &mut RunContext,
    ) -> Result<(), TestError>;
}

/// The test a spec is written for: the test itself, or the root base of a
/// test made from a base; implemented by `#[component(test..)]` on the
/// struct, which a registered test needs.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` needs `#[component(test)]` on its struct to be registered",
    label = "a test with no `#[component(test)]` on its struct"
)]
pub trait SpecTarget: Test {
    type Target: Test;
    fn __target(&mut self) -> &mut Self::Target;
    fn __config(config: &mut Self::Context) -> &mut <Self::Target as Component>::Context;
}

/// A variation of a test: settings and callbacks before it is built, typed
/// adjustments after. A test takes one spec; `BundledSpecs` layers several.
pub trait Spec<T: Test> {
    /// After `setup`, before `build`: change the configuration.
    fn pre_build(&self, _config: &mut T::Context) {}
    /// After `build`, before `run`: adjust the test, typed.
    fn post_build(&self, _test: &mut T) {}
}

/// No spec.
impl<T: Test> Spec<T> for () {}

/// Specs applied in the order given; itself a spec.
pub struct BundledSpecs<T: Test> {
    specs: Vec<Box<dyn Spec<T>>>,
}

impl<T: Test> Default for BundledSpecs<T> {
    fn default() -> Self {
        BundledSpecs { specs: Vec::new() }
    }
}

impl<T: Test> BundledSpecs<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a spec, applied after those added before it.
    pub fn with(mut self, spec: impl Spec<T> + 'static) -> Self {
        self.specs.push(Box::new(spec));
        self
    }
}

impl<T: Test> Spec<T> for BundledSpecs<T> {
    fn pre_build(&self, config: &mut T::Context) {
        for spec in &self.specs {
            spec.pre_build(config);
        }
    }
    fn post_build(&self, test: &mut T) {
        for spec in &self.specs {
            spec.post_build(test);
        }
    }
}

/// A value `SetupContext::get` converts a setting into.
pub trait FromSetting: Sized {
    fn from_setting(value: &Value) -> Result<Self, String>;
}

macro_rules! unsigned_setting {
    ($($ty:ty),*) => {$(
        impl FromSetting for $ty {
            fn from_setting(value: &Value) -> Result<Self, String> {
                let v = value.as_u64().map_err(|e| e.to_string())?;
                <$ty>::try_from(v).map_err(|_| format!("{v} does not fit `{}`", stringify!($ty)))
            }
        }
    )*};
}

unsigned_setting!(u8, u16, u32, u64, usize);

impl FromSetting for i64 {
    fn from_setting(value: &Value) -> Result<Self, String> {
        value.as_i64().map_err(|e| e.to_string())
    }
}

impl FromSetting for bool {
    fn from_setting(value: &Value) -> Result<Self, String> {
        value.as_bool_strict().map_err(|e| e.to_string())
    }
}

impl FromSetting for String {
    fn from_setting(value: &Value) -> Result<Self, String> {
        value
            .as_str()
            .map(str::to_string)
            .map_err(|e| e.to_string())
    }
}

/// What a test's `setup` is given: the settings the test module passed with
/// `tb.set(key, value)`, and logging.
pub struct SetupContext<'a> {
    start: &'a Start,
}

impl SetupContext<'_> {
    /// The setting `key`, typed; an error if it is missing or does not
    /// convert.
    pub fn get<V: FromSetting>(&self, key: &str) -> Result<V, TestError> {
        let value = self
            .start
            .setting(key)
            .ok_or_else(|| TestError::new(format!("setting `{key}` is not given")))?;
        V::from_setting(&value).map_err(|e| TestError::new(format!("setting `{key}`: {e}")))
    }

    /// The setting `key`, typed, or `default` if it is not given.
    pub fn get_or<V: FromSetting>(&self, key: &str, default: V) -> Result<V, TestError> {
        match self.start.setting(key) {
            Some(value) => {
                V::from_setting(&value).map_err(|e| TestError::new(format!("setting `{key}`: {e}")))
            }
            None => Ok(default),
        }
    }

    /// The name the test runs under.
    pub fn test_name(&self) -> &str {
        self.start.test_name()
    }
}

/// A registered test; made by `register_test!`.
#[doc(hidden)]
pub struct TestEntry {
    pub name: &'static str,
    pub start: fn(&mut Start, &'static str) -> Result<(), TestError>,
}

inventory::collect!(TestEntry);

/// Builds the test `T` with `spec` and starts its run phase: what a
/// registered test does when the test module starts it.
#[doc(hidden)]
pub fn start_test<T: SpecTarget, S: Spec<T::Target>>(
    start: &mut Start,
    name: &'static str,
    spec: S,
) -> Result<(), TestError> {
    set_phase(Phase::Build);
    let mut config = T::setup(&mut SetupContext { start })?;
    spec.pre_build(T::__config(&mut config));
    let top = Scope::new(Rc::from(""));
    let mut test: Instance<T> = top.make::<T>(Rc::from(""), config)?;
    spec.post_build(test.__target());
    let typed = test.rc().clone();
    drop(test);
    let root = top
        .into_children()
        .pop()
        .expect("the test's node was just made");

    let mut lines = Vec::new();
    root.printout(name, 0, &mut lines);
    report::message(
        crate::Severity::Debug,
        name,
        format_args!("hierarchy:\n{}", lines.join("\n")),
    );

    // A handle left unbound fails the start, naming it, rather than the
    // first sequence that would use it.
    let mut nodes = Vec::new();
    root.pre_order(&mut nodes);
    let unbound: Vec<String> = nodes
        .iter()
        .flat_map(|node| {
            let path = display(name, &node.path);
            node.component
                .borrow()
                .unbound()
                .into_iter()
                .map(move |field| format!("`{path}.{field}`"))
        })
        .collect();
    if !unbound.is_empty() {
        return Err(TestError::new(format!(
            "unbound handles: {}",
            unbound.join(", ")
        )));
    }

    // Every component's run, parents before children; the test's three
    // steps are the main task. Each first borrows its component and yields,
    // so that all are borrowed before any user code runs.
    set_phase(Phase::Run);
    // The borrow is held across the whole run on purpose, as below.
    #[allow(clippy::await_holding_refcell_ref)]
    let main = async move {
        let mut test = typed.borrow_mut();
        yield_now().await;
        let mut run_context = RunContext::new(Rc::from(name));
        Test::pre_run(&mut *test, &mut run_context).await?;
        Component::run(&mut *test, &mut run_context).await?;
        Test::post_run(&mut *test, &mut run_context).await
    };
    start.main(name, main);
    let mut nodes = Vec::new();
    root.pre_order(&mut nodes);
    for node in nodes {
        if node.passive || node.path.is_empty() {
            continue;
        }
        let component = node.component.clone();
        let path = node.path.clone();
        let owner = path.clone();
        // The borrow is held across the whole run on purpose: it is what
        // keeps the component from being reached during the run phase.
        #[allow(clippy::await_holding_refcell_ref)]
        let task = async move {
            let mut component = component.borrow_mut();
            yield_now().await;
            let mut run_context = RunContext::new(path);
            component.run(&mut run_context).await
        };
        start.spawn(&owner, task);
    }

    // Default sequences, started with the run phase.
    let mut nodes = Vec::new();
    root.pre_order(&mut nodes);
    for node in nodes {
        if let Some(task) = node.component.borrow_mut().default_task() {
            let owner = format!("{}.default_sequence", display(name, &node.path));
            start.spawn(&owner, task);
        }
    }

    start.on_end(move |end| finish(name, &root, end));
    Ok(())
}

/// The end of the run phase: `check` and `report`, children first (skipped
/// after a fatal error), the report summary, and the result.
fn finish(name: &str, root: &Node, end: &RunEnd) -> Result<(), TestError> {
    match end {
        RunEnd::Finished => {
            set_phase(Phase::Check);
            let mut nodes = Vec::new();
            root.post_order(&mut nodes);
            for node in &nodes {
                let mut check_context = CheckContext::new(display(name, &node.path));
                node.component.borrow_mut().check(&mut check_context);
            }
            for node in &nodes {
                let report_context = ReportContext::new(display(name, &node.path));
                node.component.borrow().report(&report_context);
            }
            report::summary(name, None);
            report::result()
        }
        RunEnd::Fatal { owner, error } => {
            set_phase(Phase::Check);
            report::summary(name, Some((owner, error)));
            Err(error.clone())
        }
    }
}

fn display(test: &str, path: &Rc<str>) -> Rc<str> {
    if path.is_empty() {
        Rc::from(test)
    } else {
        path.clone()
    }
}

/// The methodology's testbench: options, and the registered tests started
/// by name. `veryl_tbf::export!` exports it.
pub struct Methodology;

impl Testbench for Methodology {
    fn init() {
        report::init();
        set_phase(Phase::Build);
    }

    fn option(key: &str, value: &Value) -> Result<bool, TestError> {
        report::option(key, value)
    }

    fn options() -> Vec<String> {
        report::OPTIONS.iter().map(|o| o.to_string()).collect()
    }

    fn start(start: &mut Start) -> Result<(), TestError> {
        let name = start.test_name().to_string();
        let mut found = inventory::iter::<TestEntry>().filter(|entry| entry.name == name);
        let Some(entry) = found.next() else {
            let mut names: Vec<&str> = inventory::iter::<TestEntry>()
                .map(|entry| entry.name)
                .collect();
            names.sort_unstable();
            return Err(TestError::new(format!(
                "no test is registered as `{name}`; registered: {}",
                names.join(", ")
            )));
        };
        if found.next().is_some() {
            return Err(TestError::new(format!(
                "`{name}` is registered more than once"
            )));
        }
        (entry.start)(start, entry.name)
    }
}
