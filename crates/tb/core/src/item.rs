//! Items shared once sent: `ItemHandle<T>`.
//!
//! A sequence builds and randomizes an item as a plain value; once sent, it
//! is an `ItemHandle<T>` that the sequence, its driver and the monitors'
//! subscribers share, as UVM passes one object by handle. What the framework
//! records about an item (its ids, its begin and end) lives in the handle,
//! so an item is a plain struct of the user's.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::fmt;
use std::rc::Rc;

use verald_tb_runtime::Event;

struct Inner<T> {
    value: RefCell<T>,
    transaction_id: Cell<u64>,
    sequence_id: Cell<u64>,
    began: Cell<bool>,
    ended: Cell<bool>,
    /// Made when first asked for, as UVM's event pool makes an event on its
    /// first `get`: only an item someone waits on allocates one.
    begin_event: RefCell<Option<Event>>,
    end_event: RefCell<Option<Event>>,
}

/// An item once sent, shared by handle. Cloning shares the item.
///
/// The contents are reached with `borrow()` / `borrow_mut()`; a borrow must
/// not be held across an `.await`, where another holder's borrow would
/// panic (Clippy's `await_holding_refcell_ref` lint catches it).
pub struct ItemHandle<T> {
    inner: Rc<Inner<T>>,
}

impl<T> Clone for ItemHandle<T> {
    fn clone(&self) -> Self {
        ItemHandle {
            inner: self.inner.clone(),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for ItemHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.inner.value.try_borrow() {
            Ok(value) => value.fmt(f),
            Err(_) => f.write_str("ItemHandle(<borrowed>)"),
        }
    }
}

fn event_of(slot: &RefCell<Option<Event>>, already: bool) -> Event {
    slot.borrow_mut()
        .get_or_insert_with(|| {
            let event = Event::new();
            if already {
                event.trigger();
            }
            event
        })
        .clone()
}

/// `item.into()` where a handle is expected: `port.write(&item.into())`.
impl<T> From<T> for ItemHandle<T> {
    fn from(value: T) -> Self {
        ItemHandle::new(value)
    }
}

/// `item.into_handle()`: an item, made, put in a handle (a monitor's, to
/// publish).
pub trait IntoHandle: Sized {
    fn into_handle(self) -> ItemHandle<Self> {
        ItemHandle::new(self)
    }
}

impl<T> IntoHandle for T {}

impl<T> ItemHandle<T> {
    /// Shares `value` as a sent item.
    pub fn new(value: T) -> Self {
        ItemHandle {
            inner: Rc::new(Inner {
                value: RefCell::new(value),
                transaction_id: Cell::new(0),
                sequence_id: Cell::new(0),
                began: Cell::new(false),
                ended: Cell::new(false),
                begin_event: RefCell::new(None),
                end_event: RefCell::new(None),
            }),
        }
    }

    /// Borrows the item. Panics if it is borrowed mutably.
    pub fn borrow(&self) -> Ref<'_, T> {
        self.inner.value.borrow()
    }

    /// Borrows the item mutably. Panics if it is borrowed.
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.inner.value.borrow_mut()
    }

    /// Whether two handles share one item.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    /// The id the sequencer gave the item's transaction; responses are
    /// matched to requests by it. 0 before the item is sent.
    pub fn transaction_id(&self) -> u64 {
        self.inner.transaction_id.get()
    }

    /// The id of the sequence that sent the item; 0 if none did.
    pub fn sequence_id(&self) -> u64 {
        self.inner.sequence_id.get()
    }

    pub(crate) fn set_ids(&self, sequence_id: u64, transaction_id: u64) {
        self.inner.sequence_id.set(sequence_id);
        self.inner.transaction_id.set(transaction_id);
    }

    /// Marks the transfer begun: a driver or monitor, when it begins.
    pub fn mark_begin(&self) {
        self.inner.began.set(true);
        if let Some(event) = &*self.inner.begin_event.borrow() {
            event.trigger();
        }
    }

    /// Marks the transfer ended: a driver or monitor, when it ends.
    pub fn mark_end(&self) {
        self.inner.ended.set(true);
        if let Some(event) = &*self.inner.end_event.borrow() {
            event.trigger();
        }
    }

    pub fn began(&self) -> bool {
        self.inner.began.get()
    }

    pub fn ended(&self) -> bool {
        self.inner.ended.get()
    }

    /// The event triggered when the transfer begins (already on if it has):
    /// `item.begin_event().wait_on().await`.
    pub fn begin_event(&self) -> Event {
        event_of(&self.inner.begin_event, self.began())
    }

    /// The event triggered when the transfer ends (already on if it has):
    /// `item.end_event().wait_on().await`.
    pub fn end_event(&self) -> Event {
        event_of(&self.inner.end_event, self.ended())
    }
}
