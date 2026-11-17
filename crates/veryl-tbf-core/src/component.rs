//! Components, the tree they form, and the contexts of their phases.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::fmt;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::rc::Rc;

use veryl_component::{InputPort, ModportRef, OutputPort};
use veryl_tbf_runtime::{Clock, Reset, Signals, TestError, host, sim};

use crate::report::{self, Severity};

/// What a component is, for the hierarchy printout and logs. Components of
/// every kind are the same `Component`; the kind only names the role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Component,
    Test,
    Env,
    Agent,
    Driver,
    Monitor,
    Sequencer,
    Scoreboard,
    Subscriber,
}

/// A component of the testbench's hierarchy.
///
/// Its author writes `build`; the framework makes it, by calling `build`
/// from `Scope::create_instance` (and, for the test, when it starts). `build`
/// is not a phase: `create_instance` calls the child's `build` there and
/// then, so a parent has its child, with the child's own children, right
/// after making it.
pub trait Component: Sized + 'static {
    /// What the parent hands the component: an `InstanceContext`, handles,
    /// or a tuple of them.
    type Context;

    /// The role it plays.
    const KIND: Kind = Kind::Component;

    /// A passive component has no `run` and no run task: it is never
    /// borrowed during the run phase, so analysis ports write to it and
    /// other tasks `borrow()` it meanwhile. `#[component(.., passive)]` on
    /// the impl sets it, and requires it of a component with no `run`.
    const PASSIVE: bool = false;

    /// Makes the component and its children.
    fn build(scope: &Scope, context: Self::Context) -> Result<Self, TestError>;

    /// The run phase: every component's `run` runs concurrently; the test's
    /// ends the phase. An `Err` is a fatal error.
    async fn run(&mut self, _run_context: &mut RunContext) -> Result<(), TestError> {
        Ok(())
    }

    /// Checks what the run left, after every task has been dropped; errors
    /// go to `context`. Children are checked before their parent.
    fn check(&mut self, _check_context: &mut CheckContext) {}

    /// Summarizes, after `check`; children before their parent.
    fn report(&self, _report_context: &ReportContext) {}

    /// Told of the component's place once it is stored (a sequencer keeps
    /// it to start its default sequence).
    #[doc(hidden)]
    fn __attach(_this: &Rc<RefCell<Self>>) {}

    /// A task the component starts at the beginning of the run phase apart
    /// from its `run` (a sequencer's default sequence).
    #[doc(hidden)]
    fn __default_task(&mut self) -> Option<TaskFuture> {
        None
    }

    /// The handles of the component still unbound, by field name.
    #[doc(hidden)]
    fn __unbound(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

/// A task as the framework spawns it.
#[doc(hidden)]
pub type TaskFuture = Pin<Box<dyn Future<Output = Result<(), TestError>>>>;

/// The dyn-compatible twin of `Component`, through which the framework
/// reaches every component of the tree.
pub(crate) trait ErasedComponent {
    fn run<'a>(
        &'a mut self,
        run_context: &'a mut RunContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), TestError>> + 'a>>;
    fn check(&mut self, check_context: &mut CheckContext);
    fn report(&self, report_context: &ReportContext);
    fn default_task(&mut self) -> Option<TaskFuture>;
    fn unbound(&self) -> Vec<&'static str>;
}

impl<T: Component> ErasedComponent for T {
    fn run<'a>(
        &'a mut self,
        run_context: &'a mut RunContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), TestError>> + 'a>> {
        Box::pin(Component::run(self, run_context))
    }
    fn check(&mut self, check_context: &mut CheckContext) {
        Component::check(self, check_context)
    }
    fn report(&self, report_context: &ReportContext) {
        Component::report(self, report_context)
    }
    fn default_task(&mut self) -> Option<TaskFuture> {
        Component::__default_task(self)
    }
    fn unbound(&self) -> Vec<&'static str> {
        Component::__unbound(self)
    }
}

/// Where the testbench is, for what `Instance` allows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    Build,
    Run,
    Check,
}

thread_local! {
    static PHASE: Cell<Phase> = const { Cell::new(Phase::Build) };
    /// The message of the last build error already given a path, so that
    /// the levels above pass it on unchanged.
    static BUILD_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn set_phase(phase: Phase) {
    PHASE.with(|p| p.set(phase));
}

fn phase() -> Phase {
    PHASE.with(Cell::get)
}

/// A node of the hierarchy: one component and its children.
pub(crate) struct Node {
    pub(crate) path: Rc<str>,
    pub(crate) kind: Kind,
    pub(crate) passive: bool,

    pub(crate) component: Rc<RefCell<dyn ErasedComponent>>,
    pub(crate) children: Vec<Node>,
}

impl Node {
    /// The hierarchy, one line per component, indented by depth.
    pub(crate) fn printout(&self, test: &str, depth: usize, out: &mut Vec<String>) {
        let name = if self.path.is_empty() {
            test
        } else {
            self.path.rsplit('.').next().unwrap_or(&self.path)
        };
        let kind = format!("{:?}", self.kind).to_lowercase();
        let passive = if self.passive { ", passive" } else { "" };
        out.push(format!("{}{name} ({kind}{passive})", "  ".repeat(depth)));
        for child in &self.children {
            child.printout(test, depth + 1, out);
        }
    }

    /// The nodes, parents before children.
    pub(crate) fn pre_order<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for child in &self.children {
            child.pre_order(out);
        }
    }

    /// The nodes, children before parents.
    pub(crate) fn post_order<'a>(&'a self, out: &mut Vec<&'a Node>) {
        for child in &self.children {
            child.post_order(out);
        }
        out.push(self);
    }
}

/// A child component, held typed by its parent.
///
/// Outside the run phase it reads as the component (`Deref`):
/// `agent.sequencer()`. During the run phase each component is borrowed by
/// its own `run`, so `Deref` panics with the child's path; `borrow` /
/// `borrow_mut` reach it then, checked (a passive component, which no `run`
/// borrows).
pub struct Instance<T> {
    component: Rc<RefCell<T>>,
    path: Rc<str>,
}

impl<T> Instance<T> {
    pub(crate) fn rc(&self) -> &Rc<RefCell<T>> {
        &self.component
    }

    /// The component's path in the hierarchy (`env.agent.driver`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Borrows the component, checked: panics if its `run` holds it.
    pub fn borrow(&self) -> Ref<'_, T> {
        self.component
            .try_borrow()
            .unwrap_or_else(|_| panic!("`{}` is borrowed by its run", self.path))
    }

    /// Borrows the component mutably, checked.
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.component
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("`{}` is borrowed", self.path))
    }

    fn outside_run(&self) {
        if phase() == Phase::Run {
            panic!(
                "`{}` is reached through `Deref` during the run phase; use `borrow()`",
                self.path
            );
        }
    }
}

impl<T> Deref for Instance<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.outside_run();
        // SAFETY: outside the run phase no task holds a component, and the
        // framework borrows one only to call its `check` or `report`, never
        // while code holding a reference from here runs (a parent's `check`
        // runs after its children's). The `RefCell` is checked against a
        // mutable borrow.
        unsafe {
            self.component
                .try_borrow_unguarded()
                .unwrap_or_else(|_| panic!("`{}` is borrowed", self.path))
        }
    }
}

impl<T> DerefMut for Instance<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.outside_run();
        if self.component.try_borrow_mut().is_err() {
            panic!("`{}` is borrowed", self.path);
        }
        // SAFETY: as for `deref`, and `&mut self` excludes another
        // reference through this instance; an `Instance` is not `Clone`.
        unsafe { &mut *self.component.as_ptr() }
    }
}

impl<T> fmt::Debug for Instance<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Instance({})", self.path)
    }
}

/// A child that may be absent (a passive agent's driver).
pub type OptInstance<T> = Option<Instance<T>>;

/// What a component's `build` makes its children with: its path, the
/// clocks, resets, interfaces and signals the test module registered, and
/// logging.
pub struct Scope {
    path: Rc<str>,
    children: RefCell<Vec<Node>>,
}

impl Scope {
    pub(crate) fn new(path: Rc<str>) -> Self {
        Scope {
            path,
            children: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn into_children(self) -> Vec<Node> {
        self.children.into_inner()
    }

    /// The path of the component being built.
    pub fn path(&self) -> &str {
        &self.path
    }

    fn child_path(&self, name: &str) -> Rc<str> {
        if self.path.is_empty() {
            Rc::from(name)
        } else {
            Rc::from(format!("{}.{name}", self.path))
        }
    }

    /// Makes the child `name` of type `T` from `context`, calling its
    /// `build` now, and returns it typed.
    pub fn create_instance<T: Component>(
        &self,
        name: &str,
        context: T::Context,
    ) -> Result<Instance<T>, TestError> {
        if name.is_empty() || name.contains('.') {
            return Err(TestError::new(format!(
                "`{name}` is not a valid instance name under `{}`",
                self.path
            )));
        }
        if self
            .children
            .borrow()
            .iter()
            .any(|c| c.path.rsplit('.').next() == Some(name))
        {
            return Err(TestError::new(format!(
                "`{}` already has a child `{name}`",
                self.path
            )));
        }
        let path = self.child_path(name);
        self.make::<T>(path, context)
    }

    pub(crate) fn make<T: Component>(
        &self,
        path: Rc<str>,
        context: T::Context,
    ) -> Result<Instance<T>, TestError> {
        let scope = Scope::new(path.clone());
        let built = T::build(&scope, context).map_err(|error| {
            let message = error.message().to_string();
            let already = BUILD_ERROR.with(|b| b.borrow().as_deref() == Some(&message));
            if already {
                error
            } else {
                let shown = if path.is_empty() { "the test" } else { &path };
                let wrapped = format!("building `{shown}`: {message}");
                BUILD_ERROR.with(|b| *b.borrow_mut() = Some(wrapped.clone()));
                TestError::new(wrapped)
            }
        })?;
        let component = Rc::new(RefCell::new(built));
        T::__attach(&component);
        let erased: Rc<RefCell<dyn ErasedComponent>> = component.clone();
        self.children.borrow_mut().push(Node {
            path: path.clone(),
            kind: T::KIND,
            passive: T::PASSIVE,
            component: erased,
            children: scope.into_children(),
        });
        Ok(Instance { component, path })
    }

    /// Makes children `name[0]`, `name[1]`, .. from one context each, as UVM
    /// names arrays.
    pub fn create_instances<T: Component>(
        &self,
        name: &str,
        contexts: impl IntoIterator<Item = T::Context>,
    ) -> Result<Vec<Instance<T>>, TestError> {
        contexts
            .into_iter()
            .enumerate()
            .map(|(i, context)| {
                let path = self.child_path(&format!("{name}[{i}]"));
                self.make::<T>(path, context)
            })
            .collect()
    }

    /// Makes the child if `condition` holds.
    pub fn create_instance_if<T: Component>(
        &self,
        condition: bool,
        name: &str,
        context: T::Context,
    ) -> Result<OptInstance<T>, TestError> {
        condition
            .then(|| self.create_instance(name, context))
            .transpose()
    }

    /// The reset registered as `name`.
    pub fn reset(&self, name: &str) -> Result<Reset, TestError> {
        host::registered_reset(name)
    }

    pub fn info(&self, message: impl fmt::Display) {
        report::message(Severity::Info, &self.path, format_args!("{message}"));
    }

    pub fn warning(&self, message: impl fmt::Display) {
        report::message(Severity::Warning, &self.path, format_args!("{message}"));
    }

    pub fn error(&self, message: impl fmt::Display) {
        report::message(Severity::Error, &self.path, format_args!("{message}"));
    }
}

impl Signals for Scope {
    fn clock(&self, name: &str) -> Result<Clock, TestError> {
        host::registered_clock(name)
    }
    fn interface(&self, name: &str) -> Result<ModportRef, TestError> {
        host::registered_interface(name)
    }
    fn input(&self, path: &str) -> Result<InputPort, TestError> {
        host::registered_input(path)
    }
    fn output(&self, path: &str) -> Result<OutputPort, TestError> {
        host::registered_output(path)
    }
}

macro_rules! logging {
    ($ty:ty) => {
        impl $ty {
            /// The component's path.
            pub fn path(&self) -> &str {
                &self.path
            }

            /// Reports an error the test goes on after (`uvm_error`): the
            /// test fails.
            pub fn error(&self, message: impl fmt::Display) {
                report::message(Severity::Error, &self.path, format_args!("{message}"));
            }

            pub fn warning(&self, message: impl fmt::Display) {
                report::message(Severity::Warning, &self.path, format_args!("{message}"));
            }

            pub fn info(&self, message: impl fmt::Display) {
                report::message(Severity::Info, &self.path, format_args!("{message}"));
            }

            pub fn debug(&self, message: impl fmt::Display) {
                report::message(Severity::Debug, &self.path, format_args!("{message}"));
            }

            pub fn trace(&self, message: impl fmt::Display) {
                report::message(Severity::Trace, &self.path, format_args!("{message}"));
            }
        }
    };
}

/// What `run` is given: the path, logging, `error`, the current time.
pub struct RunContext {
    path: Rc<str>,
}

impl RunContext {
    pub(crate) fn new(path: Rc<str>) -> Self {
        RunContext { path }
    }

    /// The current simulation time.
    pub fn time(&self) -> u64 {
        sim::time()
    }
}

logging!(RunContext);

/// What `check` is given: the path, logging, `error`.
pub struct CheckContext {
    path: Rc<str>,
}

impl CheckContext {
    pub(crate) fn new(path: Rc<str>) -> Self {
        CheckContext { path }
    }
}

logging!(CheckContext);

/// What `report` is given: the path and logging.
pub struct ReportContext {
    path: Rc<str>,
}

impl ReportContext {
    pub(crate) fn new(path: Rc<str>) -> Self {
        ReportContext { path }
    }
}

logging!(ReportContext);
