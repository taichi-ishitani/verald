//! Analysis ports and their receivers.
//!
//! A port's `write` runs synchronously inside the publisher's `run`; it can
//! reach only what no `run` holds: a passive component (`connect`), a plain
//! shared object (`subscribe`), or an `AnalysisFifo` a component with a
//! `run` reads (`connect_export`).

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::rc::{Rc, Weak};
use std::task::{Poll, Waker};

use verald_tb_runtime::Fifo;

use crate::component::{Component, Instance};

/// A passive component's receiver for one item type. Ports of the same item
/// type are told apart by the id given at `connect`: an enum of the user's,
/// or `()` when there is one.
pub trait Subscriber<T> {
    type Id;
    fn write(&mut self, id: Self::Id, item: &T);
}

type Write<T> = Rc<dyn Fn(&T)>;

/// An analysis port: one writer, any number of receivers, broadcast. Clones
/// share the port, so a parent can connect a child's port it was handed.
pub struct AnalysisPort<T> {
    receivers: Rc<RefCell<Vec<Write<T>>>>,
}

impl<T> Clone for AnalysisPort<T> {
    fn clone(&self) -> Self {
        AnalysisPort {
            receivers: self.receivers.clone(),
        }
    }
}

impl<T> Default for AnalysisPort<T> {
    fn default() -> Self {
        AnalysisPort {
            receivers: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl<T: 'static> AnalysisPort<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hands `item` to every receiver, in the order they were connected.
    pub fn write(&self, item: &T) {
        // A snapshot, so that a receiver may connect another meanwhile.
        let receivers: Vec<Write<T>> = self.receivers.borrow().clone();
        for receive in receivers {
            receive(item);
        }
    }

    /// Calls `receive` with every item written: for a plain shared object
    /// (a reference model, ad-hoc logging).
    pub fn subscribe(&self, receive: impl Fn(&T) + 'static) {
        self.receivers.borrow_mut().push(Rc::new(receive));
    }

    /// Connects a passive component; its `Subscriber<T>` impl receives each
    /// item with `id`. The port holds it weakly.
    ///
    /// Panics if the component is not passive: one with a `run` is held by
    /// it for the whole run phase, so a write could not reach it.
    pub fn connect<C>(&self, receiver: &Instance<C>, id: C::Id)
    where
        C: Component + Subscriber<T>,
        C::Id: Clone + 'static,
    {
        assert!(
            C::PASSIVE,
            "`{}` has a run, so a port cannot write to it; give it an AnalysisFifo",
            receiver.path()
        );
        let weak: Weak<RefCell<C>> = Rc::downgrade(receiver.rc());
        self.subscribe(move |item| {
            if let Some(receiver) = weak.upgrade() {
                receiver.borrow_mut().write(id.clone(), item);
            }
        });
    }

    /// Connects the export of an `AnalysisFifo`, which queues a clone of
    /// each item for a component's `run` to take.
    pub fn connect_export(&self, export: &AnalysisExport<T>)
    where
        T: Clone,
    {
        let fifo = export.fifo.clone();
        self.subscribe(move |item| fifo.push(item.clone()));
    }
}

/// A FIFO an analysis port writes to and a component's `run` reads, as UVM's
/// `uvm_tlm_analysis_fifo`; the component hands out its `export()`. Clones
/// share the queue.
pub struct AnalysisFifo<T> {
    fifo: Fifo<T>,
}

impl<T> Clone for AnalysisFifo<T> {
    fn clone(&self) -> Self {
        AnalysisFifo {
            fifo: self.fifo.clone(),
        }
    }
}

impl<T> Default for AnalysisFifo<T> {
    fn default() -> Self {
        AnalysisFifo { fifo: Fifo::new() }
    }
}

impl<T> AnalysisFifo<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The handle a port connects to (`port.connect_export(&export)`).
    pub fn export(&self) -> AnalysisExport<T> {
        AnalysisExport {
            fifo: self.fifo.clone(),
        }
    }

    /// The oldest item, waiting while there is none.
    pub async fn get(&self) -> T {
        self.fifo.get().await
    }

    pub fn try_get(&self) -> Option<T> {
        self.fifo.try_get()
    }

    pub fn len(&self) -> usize {
        self.fifo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fifo.is_empty()
    }

    pub fn clear(&self) {
        self.fifo.clear()
    }
}

/// The pushing side of an `AnalysisFifo`, as UVM's `analysis_export`.
pub struct AnalysisExport<T> {
    fifo: Fifo<T>,
}

impl<T> Clone for AnalysisExport<T> {
    fn clone(&self) -> Self {
        AnalysisExport {
            fifo: self.fifo.clone(),
        }
    }
}

struct Waiting<T, K> {
    key: Option<K>,
    item: Option<T>,
    waker: Option<Waker>,
}

type Slot<T, K> = Rc<RefCell<Waiting<T, K>>>;

/// Waits for items written to the ports it is subscribed to: the next one,
/// or the next one matching a key, as tue's `tue_item_waiter`. Clones share
/// the waiter.
pub struct ItemWaiter<T, K> {
    key_of: Rc<dyn Fn(&T) -> K>,
    waiting: Rc<RefCell<Vec<Slot<T, K>>>>,
}

impl<T, K> Clone for ItemWaiter<T, K> {
    fn clone(&self) -> Self {
        ItemWaiter {
            key_of: self.key_of.clone(),
            waiting: self.waiting.clone(),
        }
    }
}

impl<T: Clone + 'static, K: PartialEq + 'static> ItemWaiter<T, K> {
    /// A waiter whose keys `key_of` takes from an item (an address, an id).
    pub fn new(key_of: impl Fn(&T) -> K + 'static) -> Self {
        ItemWaiter {
            key_of: Rc::new(key_of),
            waiting: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Subscribes the waiter to `port`.
    pub fn subscribe_to(&self, port: &AnalysisPort<T>) {
        let waiter = self.clone();
        port.subscribe(move |item| waiter.write(item));
    }

    fn write(&self, item: &T) {
        let key = (self.key_of)(item);
        let mut wakers = Vec::new();
        self.waiting.borrow_mut().retain(|slot| {
            let mut slot = slot.borrow_mut();
            let hit = slot.key.as_ref().is_none_or(|k| *k == key);
            if hit {
                slot.item = Some(item.clone());
                wakers.extend(slot.waker.take());
            }
            !hit
        });
        for waker in wakers {
            waker.wake();
        }
    }

    fn wait(&self, key: Option<K>) -> impl Future<Output = T> + 'static {
        let slot = Rc::new(RefCell::new(Waiting {
            key,
            item: None,
            waker: None,
        }));
        self.waiting.borrow_mut().push(slot.clone());
        poll_fn(move |cx| {
            let mut slot = slot.borrow_mut();
            match slot.item.take() {
                Some(item) => Poll::Ready(item),
                None => {
                    slot.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
    }

    /// The next item written after this call.
    pub fn next(&self) -> impl Future<Output = T> + 'static {
        self.wait(None)
    }

    /// The next item written after this call whose key is `key`.
    pub fn by_key(&self, key: K) -> impl Future<Output = T> + 'static {
        self.wait(Some(key))
    }
}
