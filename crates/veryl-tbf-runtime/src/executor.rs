//! The single-threaded executor.
//!
//! Tasks live in slots; a waker carries only the slot's index, so cloning one
//! is a copy and waking one pushes the index onto the ready queue. A task
//! woken again before it is polled is queued once. Waking a slot whose task
//! has ended, or that a newer task has taken, at worst polls a task once more
//! than needed, which futures allow.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::TestError;

pub(crate) type TaskFuture = Pin<Box<dyn Future<Output = Result<(), TestError>>>>;

struct Task {
    /// `None` while the task is being polled.
    future: Option<TaskFuture>,
    /// The path of the component that owns the task, for messages.
    owner: Rc<str>,
    /// Creation order, for dropping in reverse.
    order: u64,
}

#[derive(Default)]
struct Tasks {
    slots: Vec<Option<Task>>,
    free: Vec<u32>,
    next_order: u64,
}

#[derive(Default)]
struct Ready {
    queue: VecDeque<u32>,
    queued: Vec<bool>,
    /// Set while tasks are being dropped: wakes are ignored.
    closed: bool,
}

thread_local! {
    static TASKS: RefCell<Tasks> = RefCell::default();
    static READY: RefCell<Ready> = RefCell::default();
}

fn schedule(index: u32) {
    READY.with(|ready| {
        let mut ready = ready.borrow_mut();
        if ready.closed {
            return;
        }
        let i = index as usize;
        if ready.queued.len() <= i {
            ready.queued.resize(i + 1, false);
        }
        if !ready.queued[i] {
            ready.queued[i] = true;
            ready.queue.push_back(index);
        }
    });
}

fn pop_ready() -> Option<u32> {
    READY.with(|ready| {
        let mut ready = ready.borrow_mut();
        let index = ready.queue.pop_front()?;
        ready.queued[index as usize] = false;
        Some(index)
    })
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake, drop_waker);

unsafe fn clone(data: *const ()) -> RawWaker {
    RawWaker::new(data, &VTABLE)
}

unsafe fn wake(data: *const ()) {
    schedule(data as usize as u32);
}

unsafe fn drop_waker(_: *const ()) {}

fn waker(index: u32) -> Waker {
    // SAFETY: the data is a plain index, never dereferenced; every vtable
    // function is sound for any value of it.
    unsafe { Waker::from_raw(RawWaker::new(index as usize as *const (), &VTABLE)) }
}

/// A task's identity, for the run phase's main task.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TaskId(u32);

/// Adds a task and queues it for its first poll.
pub(crate) fn spawn(owner: Rc<str>, future: TaskFuture) -> TaskId {
    let index = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let order = tasks.next_order;
        tasks.next_order += 1;
        let task = Task {
            future: Some(future),
            owner,
            order,
        };
        match tasks.free.pop() {
            Some(index) => {
                tasks.slots[index as usize] = Some(task);
                index
            }
            None => {
                tasks.slots.push(Some(task));
                (tasks.slots.len() - 1) as u32
            }
        }
    });
    schedule(index);
    TaskId(index)
}

/// What polling the ready queue ended with.
pub(crate) enum Polled {
    /// The queue is empty.
    Idle,
    /// A task returned `Err`: the run phase is to end at once.
    Fatal { owner: Rc<str>, error: TestError },
    /// `stop` asked to stop polling.
    Stopped,
}

/// Polls ready tasks until the queue is empty or one fails. `on_done` is told
/// of each task that ends with `Ok`; `stop`, asked after each poll, ends the
/// polling early.
pub(crate) fn run_ready(mut on_done: impl FnMut(TaskId), stop: impl Fn() -> bool) -> Polled {
    while let Some(index) = pop_ready() {
        if stop() {
            return Polled::Stopped;
        }
        let taken = TASKS.with(|tasks| {
            let mut tasks = tasks.borrow_mut();
            tasks
                .slots
                .get_mut(index as usize)
                .and_then(Option::as_mut)
                .and_then(|task| task.future.take())
        });
        let Some(mut future) = taken else {
            continue;
        };
        let waker = waker(index);
        let mut cx = Context::from_waker(&waker);
        // The task is polled with no runtime state borrowed, so it may spawn,
        // wake and wait freely.
        let poll = future.as_mut().poll(&mut cx);
        match poll {
            Poll::Pending => TASKS.with(|tasks| {
                if let Some(Some(task)) = tasks.borrow_mut().slots.get_mut(index as usize) {
                    task.future = Some(future);
                }
            }),
            Poll::Ready(result) => {
                let owner = remove(index);
                drop(future);
                match result {
                    Ok(()) => on_done(TaskId(index)),
                    Err(error) => {
                        return Polled::Fatal {
                            owner: owner.unwrap_or_else(|| Rc::from("")),
                            error,
                        };
                    }
                }
            }
        }
    }
    Polled::Idle
}

fn remove(index: u32) -> Option<Rc<str>> {
    TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let task = tasks.slots.get_mut(index as usize)?.take()?;
        tasks.free.push(index);
        Some(task.owner)
    })
}

/// The owners of the tasks still alive, in creation order.
pub(crate) fn alive_owners() -> Vec<Rc<str>> {
    TASKS.with(|tasks| {
        let tasks = tasks.borrow();
        let mut alive: Vec<(u64, Rc<str>)> = tasks
            .slots
            .iter()
            .flatten()
            .map(|task| (task.order, task.owner.clone()))
            .collect();
        alive.sort_by_key(|(order, _)| *order);
        alive.into_iter().map(|(_, owner)| owner).collect()
    })
}

/// Drops every task, the newest first, polling none meanwhile: a `Drop` may
/// wake a task, but nothing runs. The ready queue is emptied afterwards.
pub(crate) fn drop_all() {
    READY.with(|ready| ready.borrow_mut().closed = true);
    let mut taken: Vec<(u64, Option<TaskFuture>)> = TASKS.with(|tasks| {
        let mut tasks = tasks.borrow_mut();
        let taken = tasks
            .slots
            .iter_mut()
            .filter_map(Option::take)
            .map(|task| (task.order, task.future))
            .collect();
        tasks.free.clear();
        tasks.slots.clear();
        taken
    });
    taken.sort_by_key(|(order, _)| std::cmp::Reverse(*order));
    for (_, future) in taken {
        // Dropped one by one with no state borrowed, so a `Drop` may use the
        // runtime.
        drop(future);
    }
    READY.with(|ready| {
        let mut ready = ready.borrow_mut();
        ready.queue.clear();
        ready.queued.clear();
    });
}

/// Clears every task and the ready queue, for a fresh simulation on this
/// thread.
pub(crate) fn reset() {
    drop_all();
    TASKS.with(|tasks| *tasks.borrow_mut() = Tasks::default());
    READY.with(|ready| *ready.borrow_mut() = Ready::default());
}
