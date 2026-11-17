//! A component's context (its configuration and status), knobs, and the
//! traits the `#[component]` attribute implements.

use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use crate::component::Kind;

/// A component's configuration and status, as tue's: the configuration `C`,
/// fixed after construction and shared; the status `S`, written by the
/// components (observations) and shared. Clones share both; a parent hands
/// clones to its children.
pub struct InstanceContext<C, S = ()> {
    config: Rc<C>,
    status: Rc<RefCell<S>>,
}

impl<C, S> Clone for InstanceContext<C, S> {
    fn clone(&self) -> Self {
        InstanceContext {
            config: self.config.clone(),
            status: self.status.clone(),
        }
    }
}

impl<C> InstanceContext<C, ()> {
    /// A context with a configuration and no status.
    pub fn new(config: C) -> Self {
        Self::with_status(config, ())
    }
}

impl<C, S> InstanceContext<C, S> {
    pub fn with_status(config: C, status: S) -> Self {
        InstanceContext {
            config: Rc::new(config),
            status: Rc::new(RefCell::new(status)),
        }
    }

    pub fn config(&self) -> &C {
        &self.config
    }

    /// Borrows the status; not across an `.await`.
    pub fn status(&self) -> Ref<'_, S> {
        self.status.borrow()
    }

    /// Borrows the status mutably; not across an `.await`.
    pub fn status_mut(&self) -> RefMut<'_, S> {
        self.status.borrow_mut()
    }
}

/// A setting that may change while the test runs: a field of a
/// configuration, read where it is used, every time. Clones share the value.
pub struct Knob<T> {
    value: Rc<RefCell<T>>,
}

impl<T> Clone for Knob<T> {
    fn clone(&self) -> Self {
        Knob {
            value: self.value.clone(),
        }
    }
}

impl<T: Default> Default for Knob<T> {
    fn default() -> Self {
        Knob::new(T::default())
    }
}

impl<T> Knob<T> {
    pub fn new(value: T) -> Self {
        Knob {
            value: Rc::new(RefCell::new(value)),
        }
    }

    pub fn set(&self, value: T) {
        *self.value.borrow_mut() = value;
    }
}

impl<T: Clone> Knob<T> {
    /// The value now; a copy, so no borrow is held across an `.await`.
    pub fn get(&self) -> T {
        self.value.borrow().clone()
    }
}

/// The configuration and status types of an `InstanceContext`, for the
/// generated `HasContext` (which then accepts a type alias).
#[doc(hidden)]
pub trait ContextParts {
    type Config;
    type Status;
}

impl<C, S> ContextParts for InstanceContext<C, S> {
    type Config = C;
    type Status = S;
}

/// A component that holds its context in a field marked `#[context]`;
/// implemented by `#[component(..)]` on the struct.
pub trait HasContext {
    type Config;
    type Status;
    fn context(&self) -> &InstanceContext<Self::Config, Self::Status>;
}

/// The kind a struct is declared with (`#[component(driver)]` on it), which
/// the attribute on its `impl Component` must match.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` needs `#[component(..)]` on its struct too",
    label = "`#[component(..)]` on the impl, but not on the struct"
)]
pub trait DeclaredKind {
    const KIND: Kind;
}
