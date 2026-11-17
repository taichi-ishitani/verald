//! Sequencers: the queue between sequences and a driver, arbitration, the
//! sequence item port, responses and default sequences.

use std::cell::{Ref, RefCell};
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Poll, Waker};

use veryl_tbf_runtime::TestError;

use crate::component::TaskFuture;
use crate::item::ItemHandle;
use crate::sequence::SequenceCore;

/// A small random generator for arbiters, until constrained random comes
/// with its own (step 5): SplitMix64.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `n` (which must not be 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// A sequence waiting for the driver, as an arbiter sees it.
#[derive(Clone, Debug)]
pub struct ArbRequest {
    pub sequence_id: u64,
    /// The sequence's type, for arbiters that tell sequences apart.
    pub sequence_name: &'static str,
    /// The sequence's priority (UVM's default 100).
    pub priority: i32,
    /// The sequencer's count of requests when this one began waiting.
    pub waiting_since: u64,
}

/// Picks which waiting sequence the driver gets an item from.
///
/// The sequencer calls `select` only with at least one request, in the
/// order they began waiting (the oldest at index 0), and only with the
/// eligible ones (what `grab` / `lock` would exclude is filtered out first).
/// An index out of range is a fatal error.
pub trait Arbiter: 'static {
    fn select(&mut self, requests: &[ArbRequest], rng: &mut Rng) -> usize;
}

/// UVM's `SEQ_ARB_FIFO`: the oldest request.
pub struct FifoArbiter;

impl Arbiter for FifoArbiter {
    fn select(&mut self, _requests: &[ArbRequest], _rng: &mut Rng) -> usize {
        0
    }
}

struct Waiting {
    token: u64,
    request: ArbRequest,
    waker: Option<Waker>,
}

struct State<Req, Rsp> {
    waiting: Vec<Waiting>,
    requests_seen: u64,
    /// The request granted, until its sequence sends the item.
    granted: Option<u64>,
    /// The item sent, until the driver ends it (`item_done` removes it, as
    /// UVM's removes it from its request FIFO).
    item: Option<ItemHandle<Req>>,
    taken: bool,
    /// The transactions ended, until their sequences see it.
    ended: Vec<u64>,
    finisher: Option<Waker>,
    driver: Option<Waker>,
    responses: Vec<(u64, Rsp)>,
    response_waiters: Vec<Waker>,
    next_token: u64,
    next_sequence_id: u64,
    next_transaction_id: u64,
    arbiter: Box<dyn Arbiter>,
    rng: Rng,
    default: Option<Box<dyn FnOnce() -> TaskFuture>>,
}

/// What a sequencer holds: inserted by `#[component(sequencer(..))]`.
#[doc(hidden)]
pub struct SequencerCore<Req, Rsp> {
    state: Rc<RefCell<State<Req, Rsp>>>,
    name: RefCell<Rc<str>>,
}

impl<Req, Rsp> Clone for SequencerCore<Req, Rsp> {
    fn clone(&self) -> Self {
        SequencerCore {
            state: self.state.clone(),
            name: RefCell::new(self.name.borrow().clone()),
        }
    }
}

impl<Req, Rsp> Default for SequencerCore<Req, Rsp> {
    fn default() -> Self {
        SequencerCore {
            state: Rc::new(RefCell::new(State {
                waiting: Vec::new(),
                requests_seen: 0,
                granted: None,
                item: None,
                taken: false,
                ended: Vec::new(),
                finisher: None,
                driver: None,
                responses: Vec::new(),
                response_waiters: Vec::new(),
                next_token: 1,
                next_sequence_id: 1,
                next_transaction_id: 1,
                arbiter: Box::new(FifoArbiter),
                rng: Rng::new(0),
                default: None,
            })),
            name: RefCell::new(Rc::from("")),
        }
    }
}

fn wake(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake();
    }
}

impl<Req, Rsp> SequencerCore<Req, Rsp> {
    pub(crate) fn set_name(&self, name: Rc<str>) {
        *self.name.borrow_mut() = name;
    }

    fn name(&self) -> Rc<str> {
        self.name.borrow().clone()
    }

    pub(crate) fn new_sequence_id(&self) -> u64 {
        let mut s = self.state.borrow_mut();
        let id = s.next_sequence_id;
        s.next_sequence_id += 1;
        id
    }

    /// A sequence asks for the driver; resolves when granted, with the
    /// request's token.
    pub(crate) async fn request(
        &self,
        sequence_id: u64,
        sequence_name: &'static str,
        priority: i32,
    ) -> u64 {
        let token = {
            let mut s = self.state.borrow_mut();
            let token = s.next_token;
            s.next_token += 1;
            let waiting_since = s.requests_seen;
            s.requests_seen += 1;
            s.waiting.push(Waiting {
                token,
                request: ArbRequest {
                    sequence_id,
                    sequence_name,
                    priority,
                    waiting_since,
                },
                waker: None,
            });
            let driver = s.driver.take();
            drop(s);
            wake(driver);
            token
        };
        poll_fn(|cx| {
            let mut s = self.state.borrow_mut();
            if s.granted == Some(token) {
                Poll::Ready(token)
            } else {
                if let Some(w) = s.waiting.iter_mut().find(|w| w.token == token) {
                    w.waker = Some(cx.waker().clone());
                }
                Poll::Pending
            }
        })
        .await
    }

    /// The granted sequence sends its item; resolves with the shared item
    /// once the driver has ended it.
    pub(crate) async fn send(
        &self,
        token: u64,
        sequence_id: u64,
        item: Req,
    ) -> Result<ItemHandle<Req>, TestError> {
        let handle = {
            let mut s = self.state.borrow_mut();
            if s.granted != Some(token) {
                return Err(TestError::new(format!(
                    "`{}`: `finish_item` without a granted `start_item`",
                    self.name()
                )));
            }
            let transaction_id = s.next_transaction_id;
            s.next_transaction_id += 1;
            let handle = ItemHandle::new(item);
            handle.set_ids(sequence_id, transaction_id);
            s.granted = None;
            s.item = Some(handle.clone());
            s.taken = false;
            let driver = s.driver.take();
            drop(s);
            wake(driver);
            handle
        };
        poll_fn(|cx| {
            let mut s = self.state.borrow_mut();
            let id = handle.transaction_id();
            if let Some(i) = s.ended.iter().position(|e| *e == id) {
                s.ended.swap_remove(i);
                Poll::Ready(())
            } else {
                s.finisher = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await;
        Ok(handle)
    }

    /// Grants the request the arbiter picks, if none is granted or sent.
    fn arbitrate(&self) -> Result<(), TestError> {
        let mut s = self.state.borrow_mut();
        if s.granted.is_some() || s.item.is_some() || s.waiting.is_empty() {
            return Ok(());
        }
        let requests: Vec<ArbRequest> = s.waiting.iter().map(|w| w.request.clone()).collect();
        let state = &mut *s;
        let index = state.arbiter.select(&requests, &mut state.rng);
        if index >= state.waiting.len() {
            return Err(TestError::new(format!(
                "`{}`: the arbiter chose request {index} of {}",
                self.name(),
                state.waiting.len()
            )));
        }
        let granted = state.waiting.remove(index);
        state.granted = Some(granted.token);
        drop(s);
        wake(granted.waker);
        Ok(())
    }

    /// The driver takes the next item, waiting for one.
    pub(crate) async fn next_item(&self) -> Result<ItemHandle<Req>, TestError> {
        if self.state.borrow().taken {
            return Err(TestError::new(format!(
                "`{}`: `get_next_item` before `item_done`",
                self.name()
            )));
        }
        loop {
            if let Some(item) = self.take_item() {
                return Ok(item);
            }
            if self.needs_grant() {
                // As UVM's `wait_for_sequences`: let the sequences woken in
                // this hook (one whose item just ended) ask before choosing.
                veryl_tbf_runtime::yield_now().await;
                self.arbitrate()?;
            }
            // Wait for a request or an item.
            let mut registered = false;
            poll_fn(|cx| {
                if registered {
                    Poll::Ready(())
                } else {
                    registered = true;
                    self.state.borrow_mut().driver = Some(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await;
        }
    }

    /// Takes the item sent, if the driver has not taken it.
    fn take_item(&self) -> Option<ItemHandle<Req>> {
        let mut s = self.state.borrow_mut();
        if s.taken {
            return None;
        }
        let item = s.item.clone()?;
        s.taken = true;
        Some(item)
    }

    /// Whether the driver has to grant a request to get an item.
    fn needs_grant(&self) -> bool {
        let s = self.state.borrow();
        s.granted.is_none() && s.item.is_none() && !s.waiting.is_empty()
    }

    /// The driver takes the next item if a sequence has one now: a sequence
    /// waiting is granted and given the hook's remaining turns to send it.
    pub(crate) async fn try_next_item(&self) -> Result<Option<ItemHandle<Req>>, TestError> {
        if self.state.borrow().taken {
            return Err(TestError::new(format!(
                "`{}`: `try_next_item` before `item_done`",
                self.name()
            )));
        }
        // As UVM's: let the sequences woken in this hook ask first.
        veryl_tbf_runtime::yield_now().await;
        self.arbitrate()?;
        loop {
            if let Some(item) = self.take_item() {
                return Ok(Some(item));
            }
            if self.state.borrow().granted.is_none() {
                return Ok(None);
            }
            veryl_tbf_runtime::yield_now().await;
        }
    }

    /// The driver ends the item it took.
    pub(crate) fn item_done(&self) -> Result<(), TestError> {
        let mut s = self.state.borrow_mut();
        if !s.taken {
            return Err(TestError::new(format!(
                "`{}`: `item_done` with no item taken",
                self.name()
            )));
        }
        s.taken = false;
        let ended = s.item.take().expect("an item is taken");
        s.ended.push(ended.transaction_id());
        let finisher = s.finisher.take();
        drop(s);
        wake(finisher);
        Ok(())
    }

    pub(crate) fn put_response(&self, transaction_id: u64, response: Rsp) {
        let waiters = {
            let mut s = self.state.borrow_mut();
            s.responses.push((transaction_id, response));
            std::mem::take(&mut s.response_waiters)
        };
        for waker in waiters {
            waker.wake();
        }
    }

    pub(crate) async fn response(&self, transaction_id: u64) -> Rsp {
        poll_fn(|cx| {
            let mut s = self.state.borrow_mut();
            match s.responses.iter().position(|(id, _)| *id == transaction_id) {
                Some(i) => Poll::Ready(s.responses.remove(i).1),
                None => {
                    s.response_waiters.push(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await
    }

    pub(crate) fn set_arbiter(&self, arbiter: impl Arbiter) {
        self.state.borrow_mut().arbiter = Box::new(arbiter);
    }

    pub(crate) fn set_default(&self, task: Box<dyn FnOnce() -> TaskFuture>) {
        self.state.borrow_mut().default = Some(task);
    }

    #[doc(hidden)]
    pub fn take_default(&self) -> Option<TaskFuture> {
        let task = self.state.borrow_mut().default.take()?;
        Some(task())
    }
}

/// A sequencer: a component holding a `SequencerCore`, implemented by
/// `#[component(sequencer(item = ..))]`.
pub trait Sequencer: Sized + 'static {
    /// The items its sequences send.
    type Item: 'static;
    /// The responses its driver returns (the item's type unless given).
    type Response: 'static;
    #[doc(hidden)]
    fn __core(&self) -> &SequencerCore<Self::Item, Self::Response>;
}

impl<S: Sequencer> crate::component::Instance<S> {
    /// A handle to the sequencer, for sequences to start on and for its
    /// driver's port to connect to.
    pub fn handle(&self) -> SequencerHandle<S> {
        let handle = SequencerHandle::new(self.rc().clone());
        handle.core().set_name(Rc::from(self.path()));
        handle
    }
}

/// A typed handle to a sequencer, which sequences start on and keep: a
/// cheap clone. A handle starts unbound (`Default`); the framework fails
/// the start, naming it, if one is left unbound in a component.
pub struct SequencerHandle<S: Sequencer> {
    bound: Option<Bound<S>>,
}

/// A bound handle's sequencer and its core.
type Bound<S> = (
    Rc<RefCell<S>>,
    SequencerCore<<S as Sequencer>::Item, <S as Sequencer>::Response>,
);

impl<S: Sequencer> Clone for SequencerHandle<S> {
    fn clone(&self) -> Self {
        SequencerHandle {
            bound: self.bound.clone(),
        }
    }
}

impl<S: Sequencer> Default for SequencerHandle<S> {
    fn default() -> Self {
        SequencerHandle { bound: None }
    }
}

impl<S: Sequencer> SequencerHandle<S> {
    pub(crate) fn new(sequencer: Rc<RefCell<S>>) -> Self {
        let core = sequencer.borrow().__core().clone();
        SequencerHandle {
            bound: Some((sequencer, core)),
        }
    }

    pub fn is_bound(&self) -> bool {
        self.bound.is_some()
    }

    fn bound(&self) -> &Bound<S> {
        self.bound
            .as_ref()
            .expect("the sequencer handle is unbound")
    }

    pub(crate) fn core(&self) -> &SequencerCore<S::Item, S::Response> {
        &self.bound().1
    }

    /// Borrows the sequencer, typed: its fields (a virtual sequencer's
    /// sub-sequencers, its context). Not across an `.await`.
    pub fn borrow(&self) -> Ref<'_, S> {
        self.bound().0.borrow()
    }

    /// Replaces the arbitration (UVM's `set_arbitration`), at any time.
    pub fn set_arbiter(&self, arbiter: impl Arbiter) {
        self.core().set_arbiter(arbiter);
    }

    /// Sets the sequence the sequencer starts at the beginning of the run
    /// phase, built by `make`; it runs in the background and is dropped with
    /// the other tasks when the run phase ends.
    pub fn set_default_sequence<Q>(&self, make: impl FnOnce() -> Q + 'static)
    where
        Q: SequenceCore<Sequencer = S> + 'static,
    {
        let handle = self.clone();
        self.core().set_default(Box::new(move || {
            Box::pin(async move {
                let mut sequence = make();
                sequence.start(&handle).await
            })
        }));
    }
}

/// A driver's port to its sequencer (UVM's `seq_item_port`).
pub struct SeqItemPort<Req, Rsp = Req> {
    core: Option<SequencerCore<Req, Rsp>>,
}

impl<Req, Rsp> Default for SeqItemPort<Req, Rsp> {
    fn default() -> Self {
        SeqItemPort { core: None }
    }
}

impl<Req: 'static, Rsp: 'static> SeqItemPort<Req, Rsp> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects the port to a sequencer of the same item and response.
    pub fn connect<S>(&mut self, sequencer: &SequencerHandle<S>)
    where
        S: Sequencer<Item = Req, Response = Rsp>,
    {
        self.core = Some(sequencer.core().clone());
    }

    pub fn is_connected(&self) -> bool {
        self.core.is_some()
    }

    fn core(&self) -> Result<&SequencerCore<Req, Rsp>, TestError> {
        self.core
            .as_ref()
            .ok_or_else(|| TestError::new("the sequence item port is not connected"))
    }

    /// The next item, waiting for a sequence to send one.
    pub async fn get_next_item(&self) -> Result<ItemHandle<Req>, TestError> {
        self.core()?.next_item().await
    }

    /// The next item if a sequence has one now.
    pub async fn try_next_item(&self) -> Result<Option<ItemHandle<Req>>, TestError> {
        self.core()?.try_next_item().await
    }

    /// Ends the item taken; its sequence's `finish_item` returns.
    pub fn item_done(&self) -> Result<(), TestError> {
        self.core()?.item_done()
    }

    /// Returns a response to the sequence that sent `request`, matched by
    /// its transaction id.
    pub fn put_response(&self, request: &ItemHandle<Req>, response: Rsp) -> Result<(), TestError> {
        self.core()?
            .put_response(request.transaction_id(), response);
        Ok(())
    }
}
