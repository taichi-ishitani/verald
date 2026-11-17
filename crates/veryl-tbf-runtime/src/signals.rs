//! Clocks and resets, and waiting for them.
//!
//! A clock keeps the tasks waiting for its next edge in a list that is
//! reused from edge to edge, so waiting allocates nothing once warmed up; an
//! edge wakes exactly those tasks.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use veryl_component::{ClockPort, ResetPort};

use crate::state::with_state;

pub(crate) struct ClockState {
    pub(crate) name: String,
    pub(crate) port: ClockPort,
    /// Edges seen since the start.
    pub(crate) edges: u64,
    pub(crate) waiters: Vec<Waker>,
    /// The previous edge's list, kept for its capacity.
    pub(crate) spare: Vec<Waker>,
}

impl ClockState {
    pub(crate) fn new(name: String, port: ClockPort) -> Self {
        ClockState {
            name,
            port,
            edges: 0,
            waiters: Vec::new(),
            spare: Vec::new(),
        }
    }

    /// Counts an edge and wakes the tasks waiting for it.
    pub(crate) fn edge(&mut self) -> Vec<Waker> {
        self.edges += 1;
        std::mem::swap(&mut self.waiters, &mut self.spare);
        std::mem::take(&mut self.spare)
    }

    /// Gives back a list emptied after waking, for its capacity.
    pub(crate) fn recycle(&mut self, mut list: Vec<Waker>) {
        list.clear();
        if self.spare.capacity() < list.capacity() {
            self.spare = list;
        }
    }
}

pub(crate) struct ResetState {
    pub(crate) name: String,
    pub(crate) port: ResetPort,
    pub(crate) asserted: bool,
    pub(crate) waiters: Vec<Waker>,
}

impl ResetState {
    pub(crate) fn new(name: String, port: ResetPort) -> Self {
        ResetState {
            name,
            port,
            asserted: false,
            waiters: Vec::new(),
        }
    }

    /// Sets the state; returns the tasks to wake if it changed.
    pub(crate) fn set(&mut self, asserted: bool) -> Vec<Waker> {
        if self.asserted == asserted {
            Vec::new()
        } else {
            self.asserted = asserted;
            std::mem::take(&mut self.waiters)
        }
    }
}

/// A clock registered by the test module (`tb.set_clock("clk", clk)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Clock {
    index: usize,
}

impl Clock {
    pub(crate) fn new(index: usize) -> Self {
        Clock { index }
    }

    /// The name the clock was registered under.
    pub fn name(&self) -> String {
        with_state(|s| s.clocks[self.index].name.clone())
    }

    /// The edges seen so far.
    pub fn edges(&self) -> u64 {
        with_state(|s| s.clocks[self.index].edges)
    }

    /// Waits for the next edge.
    pub fn edge(&self) -> Edges {
        self.wait(1)
    }

    /// Waits for `count` edges; `wait(0)` is ready at once.
    pub fn wait(&self, count: u64) -> Edges {
        Edges {
            index: self.index,
            count,
            target: None,
            registered: None,
        }
    }
}

/// Waits for a number of edges of a clock.
#[must_use = "a wait does nothing unless awaited"]
pub struct Edges {
    index: usize,
    count: u64,
    target: Option<u64>,
    /// The edge count when the waker was last registered: a registration
    /// holds until the next edge, so polling again before it adds none.
    registered: Option<u64>,
}

impl Future for Edges {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let index = self.index;
        let count = self.count;
        let target = *self
            .target
            .get_or_insert_with(|| with_state(|s| s.clocks[index].edges) + count);
        let registered = self.registered;
        let (poll, now) = with_state(|s| {
            let clock = &mut s.clocks[index];
            if clock.edges >= target {
                (Poll::Ready(()), clock.edges)
            } else {
                // Woken at each edge until the count is reached.
                if registered != Some(clock.edges) {
                    clock.waiters.push(cx.waker().clone());
                }
                (Poll::Pending, clock.edges)
            }
        });
        if poll.is_pending() {
            self.registered = Some(now);
        }
        poll
    }
}

/// A reset registered by the test module (`tb.set_reset("rst", rst)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reset {
    index: usize,
}

impl Reset {
    pub(crate) fn new(index: usize) -> Self {
        Reset { index }
    }

    /// The name the reset was registered under.
    pub fn name(&self) -> String {
        with_state(|s| s.resets[self.index].name.clone())
    }

    pub fn is_asserted(&self) -> bool {
        with_state(|s| s.resets[self.index].asserted)
    }

    /// Resolves once the reset is asserted; at once if it is.
    pub fn asserted(&self) -> ResetWait {
        ResetWait {
            index: self.index,
            asserted: true,
        }
    }

    /// Resolves once the reset is released; at once if it is.
    pub fn released(&self) -> ResetWait {
        ResetWait {
            index: self.index,
            asserted: false,
        }
    }
}

/// Waits for a reset to reach a state.
#[must_use = "a wait does nothing unless awaited"]
pub struct ResetWait {
    index: usize,
    asserted: bool,
}

impl Future for ResetWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        with_state(|s| {
            let reset = &mut s.resets[self.index];
            if reset.asserted == self.asserted {
                Poll::Ready(())
            } else {
                reset.waiters.push(cx.waker().clone());
                Poll::Pending
            }
        })
    }
}
