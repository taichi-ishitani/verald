//! Running futures together within a task: SystemVerilog's `fork` forms.
//!
//! | SystemVerilog       | here                                   |
//! |---------------------|----------------------------------------|
//! | `fork .. join`      | [`join`], [`try_join`]                 |
//! | `fork .. join_any`  | [`first`] (the other is dropped)       |
//! | `fork .. join_none` | [`ForkScope::fork`] inside a [`fork_scope`] |
//! | `disable fork`      | dropping the scope                     |

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;

/// Yields once: the task goes to the back of the ready queue and resumes in
/// the same hook, after the tasks already ready.
pub async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

/// Runs two futures together; returns both outputs.
pub async fn join<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut out_a = None;
    let mut out_b = None;
    poll_fn(|cx| {
        if out_a.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            out_a = Some(v);
        }
        if out_b.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            out_b = Some(v);
        }
        if out_a.is_some() && out_b.is_some() {
            Poll::Ready((out_a.take().unwrap(), out_b.take().unwrap()))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// Runs two fallible futures together; returns both outputs, or the first
/// error, dropping the other future.
pub async fn try_join<A, B, TA, TB, E>(a: A, b: B) -> Result<(TA, TB), E>
where
    A: Future<Output = Result<TA, E>>,
    B: Future<Output = Result<TB, E>>,
{
    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut out_a = None;
    let mut out_b = None;
    poll_fn(|cx| {
        if out_a.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            match v {
                Ok(v) => out_a = Some(v),
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
        if out_b.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            match v {
                Ok(v) => out_b = Some(v),
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
        if out_a.is_some() && out_b.is_some() {
            Poll::Ready(Ok((out_a.take().unwrap(), out_b.take().unwrap())))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// The output of [`first`]: which future ended first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Either<A, B> {
    First(A),
    Second(B),
}

/// Runs two futures until one ends; the other is dropped. When both are
/// ready at the same poll, the first wins.
pub async fn first<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(v) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::First(v));
        }
        if let Poll::Ready(v) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Second(v));
        }
        Poll::Pending
    })
    .await
}

type Forked<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// The tasks forked in a [`fork_scope`].
pub struct ForkScope<'a> {
    running: RefCell<Vec<Forked<'a>>>,
    /// Forked since the last poll; moved to `running` by the scope.
    added: RefCell<Vec<Forked<'a>>>,
}

impl<'a> ForkScope<'a> {
    /// Starts `task` alongside the scope's body, as SystemVerilog's
    /// `fork .. join_none`: it runs until it ends or the scope is dropped.
    pub fn fork(&self, task: impl Future<Output = ()> + 'a) {
        self.added.borrow_mut().push(Box::pin(task));
    }
}

/// Runs `body` with a scope to fork tasks in. The forked tasks are owned by
/// the scope and polled along with the body, so they may borrow what
/// outlives the scope and need no `'static`. The scope ends when the body
/// and every forked task have ended; dropping it drops them all (a reset
/// dropping a driver's work drops what it forked).
pub async fn fork_scope<'a, R>(body: impl AsyncFnOnce(&ForkScope<'a>) -> R) -> R {
    let scope = ForkScope {
        running: RefCell::new(Vec::new()),
        added: RefCell::new(Vec::new()),
    };
    let mut body = pin!(body(&scope));
    let mut output = None;
    poll_fn(|cx| {
        if output.is_none()
            && let Poll::Ready(v) = body.as_mut().poll(cx)
        {
            output = Some(v);
        }
        let mut running = scope.running.borrow_mut();
        let mut i = 0;
        loop {
            // Tasks forked by the body or by another task join the run.
            running.append(&mut scope.added.borrow_mut());
            if i >= running.len() {
                break;
            }
            if running[i].as_mut().poll(cx).is_ready() {
                drop(running.remove(i));
            } else {
                i += 1;
            }
        }
        if output.is_some() && running.is_empty() {
            Poll::Ready(output.take().unwrap())
        } else {
            Poll::Pending
        }
    })
    .await
}
