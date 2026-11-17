//! Parts that tasks share: events and FIFOs.
//!
//! They work through `&self`, with a `RefCell` inside that is borrowed only
//! within one call or one poll, never across an `.await`; the executor is
//! single-threaded, so the borrows never overlap.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::rc::Rc;
use std::task::{Poll, Waker};

fn wake_all(waiters: Vec<Waker>) {
    for waker in waiters {
        waker.wake();
    }
}

struct EventInner<T> {
    on: bool,
    data: Option<T>,
    /// Counts triggers, so that a wait for the next one tells it apart.
    triggers: u64,
    waiters: Vec<Waker>,
}

/// An event, as UVM's `uvm_event`: triggered, possibly with data, and waited
/// for. Clones share the event.
pub struct Event<T: Clone = ()> {
    inner: Rc<RefCell<EventInner<T>>>,
}

impl<T: Clone> Clone for Event<T> {
    fn clone(&self) -> Self {
        Event {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone> Default for Event<T> {
    fn default() -> Self {
        Event {
            inner: Rc::new(RefCell::new(EventInner {
                on: false,
                data: None,
                triggers: 0,
                waiters: Vec::new(),
            })),
        }
    }
}

impl<T: Clone> Event<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns the event on with `data` and wakes every waiter.
    pub fn trigger_with(&self, data: T) {
        let waiters = {
            let mut inner = self.inner.borrow_mut();
            inner.on = true;
            inner.data = Some(data);
            inner.triggers += 1;
            std::mem::take(&mut inner.waiters)
        };
        wake_all(waiters);
    }

    /// Turns the event off; later `wait_on` calls wait again.
    pub fn reset(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.on = false;
        inner.data = None;
    }

    pub fn is_on(&self) -> bool {
        self.inner.borrow().on
    }

    /// The data of the last trigger.
    pub fn data(&self) -> Option<T> {
        self.inner.borrow().data.clone()
    }

    /// Waits until the event is on (at once if it is), as UVM's `wait_on`;
    /// returns the trigger's data.
    pub fn wait_on(&self) -> impl Future<Output = T> + '_ {
        poll_fn(move |cx| {
            let mut inner = self.inner.borrow_mut();
            match (&inner.on, &inner.data) {
                (true, Some(data)) => Poll::Ready(data.clone()),
                _ => {
                    inner.waiters.push(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
    }

    /// Waits for the next trigger, as UVM's `wait_trigger`; returns its data.
    pub fn wait_trigger(&self) -> impl Future<Output = T> + '_ {
        let start = self.inner.borrow().triggers;
        poll_fn(move |cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.triggers > start {
                Poll::Ready(inner.data.clone().expect("a triggered event has data"))
            } else {
                inner.waiters.push(cx.waker().clone());
                Poll::Pending
            }
        })
    }
}

impl Event<()> {
    /// Turns the event on and wakes every waiter.
    pub fn trigger(&self) {
        self.trigger_with(());
    }
}

struct FifoInner<T> {
    items: VecDeque<T>,
    /// 0 for no limit.
    capacity: usize,
    getters: Vec<Waker>,
    putters: Vec<Waker>,
}

/// A FIFO shared by tasks, optionally bounded: `put` waits while it is full,
/// `get` while it is empty. Clones share the FIFO.
pub struct Fifo<T> {
    inner: Rc<RefCell<FifoInner<T>>>,
}

impl<T> Clone for Fifo<T> {
    fn clone(&self) -> Self {
        Fifo {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Default for Fifo<T> {
    fn default() -> Self {
        Self::bounded(0)
    }
}

impl<T> Fifo<T> {
    /// A FIFO with no limit.
    pub fn new() -> Self {
        Self::default()
    }

    /// A FIFO holding at most `capacity` items; 0 for no limit.
    pub fn bounded(capacity: usize) -> Self {
        Fifo {
            inner: Rc::new(RefCell::new(FifoInner {
                items: VecDeque::new(),
                capacity,
                getters: Vec::new(),
                putters: Vec::new(),
            })),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.borrow().capacity
    }

    pub fn len(&self) -> usize {
        self.inner.borrow().items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.borrow().items.is_empty()
    }

    pub fn is_full(&self) -> bool {
        let inner = self.inner.borrow();
        inner.capacity > 0 && inner.items.len() >= inner.capacity
    }

    /// Adds an item unless the FIFO is full; gives it back if it is.
    pub fn try_put(&self, item: T) -> Result<(), T> {
        let getters = {
            let mut inner = self.inner.borrow_mut();
            if inner.capacity > 0 && inner.items.len() >= inner.capacity {
                return Err(item);
            }
            inner.items.push_back(item);
            std::mem::take(&mut inner.getters)
        };
        wake_all(getters);
        Ok(())
    }

    /// Adds an item, waiting while the FIFO is full.
    pub async fn put(&self, item: T) {
        let mut item = Some(item);
        poll_fn(|cx| match self.try_put(item.take().expect("put once")) {
            Ok(()) => Poll::Ready(()),
            Err(back) => {
                item = Some(back);
                self.inner.borrow_mut().putters.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Adds an item to a FIFO with no limit; panics on a bounded one that is
    /// full, which `put` would wait on.
    pub fn push(&self, item: T) {
        if self.try_put(item).is_err() {
            panic!("pushed to a full bounded FIFO; use `put` to wait for room");
        }
    }

    /// Takes the oldest item, if any.
    pub fn try_get(&self) -> Option<T> {
        let (item, putters) = {
            let mut inner = self.inner.borrow_mut();
            let item = inner.items.pop_front()?;
            (item, std::mem::take(&mut inner.putters))
        };
        wake_all(putters);
        Some(item)
    }

    /// Takes the oldest item, waiting while the FIFO is empty.
    pub async fn get(&self) -> T {
        poll_fn(|cx| match self.try_get() {
            Some(item) => Poll::Ready(item),
            None => {
                self.inner.borrow_mut().getters.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Removes every item.
    pub fn clear(&self) {
        let putters = {
            let mut inner = self.inner.borrow_mut();
            inner.items.clear();
            std::mem::take(&mut inner.putters)
        };
        wake_all(putters);
    }
}

impl<T: Clone> Fifo<T> {
    /// The oldest item, left in place, if any.
    pub fn try_peek(&self) -> Option<T> {
        self.inner.borrow().items.front().cloned()
    }

    /// The oldest item, left in place, waiting while the FIFO is empty.
    pub async fn peek(&self) -> T {
        poll_fn(|cx| match self.try_peek() {
            Some(item) => Poll::Ready(item),
            None => {
                self.inner.borrow_mut().getters.push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
