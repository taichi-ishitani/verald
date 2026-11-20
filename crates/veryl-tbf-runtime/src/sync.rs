//! Parts that tasks share: events, FIFOs and semaphores.
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

/// A request waiting in a semaphore's queue.
struct Request {
    keys: usize,
    /// Set when the keys are handed to the request.
    granted: bool,
    waker: Option<Waker>,
}

struct SemaphoreInner {
    available: usize,
    /// The requests waiting, oldest first.
    queue: VecDeque<Rc<RefCell<Request>>>,
}

impl SemaphoreInner {
    /// Hands keys to the waiting requests in order, while the oldest one can
    /// be served; returns the wakers of those served.
    fn serve(&mut self) -> Vec<Waker> {
        let mut served = Vec::new();
        while let Some(front) = self.queue.front() {
            let mut request = front.borrow_mut();
            if request.keys > self.available {
                break;
            }
            self.available -= request.keys;
            request.granted = true;
            served.extend(request.waker.take());
            drop(request);
            self.queue.pop_front();
        }
        served
    }
}

/// A semaphore, as SystemVerilog's `semaphore`: a number of keys that tasks
/// take and give back. Clones share the semaphore.
///
/// Taking keys gives a [`Permit`], given back with [`Semaphore::release`].
/// A permit dropped instead gives its keys back too, so a task dropped while
/// it holds keys (at the end of the run phase, or the losing side of
/// `first`) does not keep them. Requests are served in the order they were
/// made: one that cannot be served yet holds back those after it, so the
/// order is the same in every run.
pub struct Semaphore {
    inner: Rc<RefCell<SemaphoreInner>>,
}

impl Clone for Semaphore {
    fn clone(&self) -> Self {
        Semaphore {
            inner: self.inner.clone(),
        }
    }
}

impl Semaphore {
    /// A semaphore with `keys` keys, as SystemVerilog's `new(keys)`.
    pub fn new(keys: usize) -> Self {
        Semaphore {
            inner: Rc::new(RefCell::new(SemaphoreInner {
                available: keys,
                queue: VecDeque::new(),
            })),
        }
    }

    /// The keys free now.
    pub fn available(&self) -> usize {
        self.inner.borrow().available
    }

    /// Takes `keys` keys, waiting until they are free and every earlier
    /// request is served, as SystemVerilog's `get(keys)`.
    pub async fn acquire(&self, keys: usize) -> Permit {
        if let Some(permit) = self.try_acquire(keys) {
            return permit;
        }
        let request = Rc::new(RefCell::new(Request {
            keys,
            granted: false,
            waker: None,
        }));
        self.inner.borrow_mut().queue.push_back(request.clone());
        let mut waiting = Waiting {
            semaphore: self,
            request,
            taken: false,
        };
        poll_fn(|cx| {
            let mut request = waiting.request.borrow_mut();
            if request.granted {
                Poll::Ready(())
            } else {
                request.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
        waiting.taken = true;
        self.permit(keys)
    }

    /// Takes `keys` keys if they are free and no request waits before, as
    /// SystemVerilog's `try_get(keys)`.
    pub fn try_acquire(&self, keys: usize) -> Option<Permit> {
        let mut inner = self.inner.borrow_mut();
        if !inner.queue.is_empty() || inner.available < keys {
            return None;
        }
        inner.available -= keys;
        Some(self.permit(keys))
    }

    /// Gives the keys of `permit` back, as SystemVerilog's `put(keys)`, and
    /// serves the requests waiting. Panics on a permit of another semaphore.
    pub fn release(&self, permit: Permit) {
        assert!(
            Rc::ptr_eq(&self.inner, &permit.semaphore.inner),
            "a permit is released to another semaphore than the one it came from"
        );
        drop(permit);
    }

    /// Adds `keys` keys, as SystemVerilog's `put(keys)` beyond those taken.
    pub fn add(&self, keys: usize) {
        self.give_back(keys);
    }

    fn permit(&self, keys: usize) -> Permit {
        Permit {
            semaphore: self.clone(),
            keys,
        }
    }

    fn give_back(&self, keys: usize) {
        let served = {
            let mut inner = self.inner.borrow_mut();
            inner.available += keys;
            inner.serve()
        };
        wake_all(served);
    }
}

/// Keys taken from a [`Semaphore`], given back with [`Semaphore::release`],
/// or when it is dropped.
#[must_use = "the keys go back at once when the permit is dropped"]
pub struct Permit {
    semaphore: Semaphore,
    keys: usize,
}

impl Permit {
    /// The keys it holds.
    pub fn keys(&self) -> usize {
        self.keys
    }

    /// Keeps the keys taken: they do not go back when the permit is gone.
    pub fn forget(mut self) {
        self.keys = 0;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.keys > 0 {
            self.semaphore.give_back(self.keys);
        }
    }
}

/// A request of [`Semaphore::acquire`] in the queue. Dropped before it is
/// served (the wait dropped), it leaves the queue; served but not taken, it
/// gives the keys back.
struct Waiting<'a> {
    semaphore: &'a Semaphore,
    request: Rc<RefCell<Request>>,
    taken: bool,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if self.taken {
            return;
        }
        let (granted, keys) = {
            let request = self.request.borrow();
            (request.granted, request.keys)
        };
        if granted {
            self.semaphore.give_back(keys);
        } else {
            let served = {
                let mut inner = self.semaphore.inner.borrow_mut();
                inner.queue.retain(|r| !Rc::ptr_eq(r, &self.request));
                inner.serve()
            };
            wake_all(served);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::Context;

    fn poll<F: Future>(future: &mut Pin<Box<F>>) -> Poll<F::Output> {
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn keys_are_taken_and_given_back() {
        let semaphore = Semaphore::new(2);
        let a = semaphore.try_acquire(1).unwrap();
        let b = semaphore.try_acquire(1).unwrap();
        assert!(semaphore.try_acquire(1).is_none());
        semaphore.release(a);
        assert_eq!(semaphore.available(), 1);
        b.forget();
        assert_eq!(semaphore.available(), 1);
        semaphore.add(2);
        assert_eq!(semaphore.available(), 3);
        assert_eq!(semaphore.try_acquire(0).unwrap().keys(), 0);
    }

    #[test]
    fn requests_are_served_in_order() {
        let semaphore = Semaphore::new(1);
        let held = semaphore.try_acquire(1).unwrap();
        let mut big = Box::pin(semaphore.acquire(2));
        let mut small = Box::pin(semaphore.acquire(1));
        assert!(poll(&mut big).is_pending());
        assert!(poll(&mut small).is_pending());
        // A key back: the oldest request needs two, so neither is served,
        // and `try_acquire` does not pass the queue either.
        semaphore.release(held);
        assert!(semaphore.try_acquire(1).is_none());
        assert!(poll(&mut small).is_pending());
        semaphore.add(2);
        let Poll::Ready(big) = poll(&mut big) else {
            panic!("the oldest request is served first");
        };
        let Poll::Ready(small) = poll(&mut small) else {
            panic!("then the next");
        };
        assert_eq!((big.keys(), small.keys()), (2, 1));
        assert_eq!(semaphore.available(), 0);
    }

    #[test]
    fn a_dropped_wait_leaves_the_queue_or_gives_back() {
        let semaphore = Semaphore::new(1);
        let held = semaphore.try_acquire(1).unwrap();
        let mut big = Box::pin(semaphore.acquire(2));
        let mut small = Box::pin(semaphore.acquire(1));
        assert!(poll(&mut big).is_pending());
        assert!(poll(&mut small).is_pending());
        // A permit dropped, not released, gives its keys back too.
        drop(held);
        // Leaving the queue lets the request after it in.
        drop(big);
        assert!(matches!(poll(&mut small), Poll::Ready(_)));
        // A request served but dropped before it is polled gives back.
        let semaphore = Semaphore::new(0);
        let mut wait = Box::pin(semaphore.acquire(1));
        assert!(poll(&mut wait).is_pending());
        semaphore.add(1);
        assert_eq!(semaphore.available(), 0);
        drop(wait);
        assert_eq!(semaphore.available(), 1);
    }

    #[test]
    #[should_panic(expected = "another semaphore")]
    fn a_permit_goes_back_to_its_own_semaphore() {
        let a = Semaphore::new(1);
        let b = Semaphore::new(1);
        let permit = a.try_acquire(1).unwrap();
        b.release(permit);
    }
}
