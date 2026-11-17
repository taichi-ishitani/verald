//! Sequences: the body the user writes (`Sequence`), the core the framework
//! gives every sequence (`SequenceCore`, implemented by `#[sequence]`), and
//! a protocol's fields (`HasBase`).

use std::cell::Ref;

use std::rc::Rc;

use veryl_tbf_random::{Constraints, RandError, Randomize, seeded};
use veryl_tbf_runtime::TestError;

use crate::context::HasContext;
use crate::item::ItemHandle;
use crate::seeds;
use crate::sequencer::{Sequencer, SequencerHandle};

/// What a sequence does: its body. The rest is `SequenceCore`.
pub trait Sequence {
    async fn body(&mut self) -> Result<(), TestError>;
}

/// The fields `#[sequence]` inserts for the core.
#[doc(hidden)]
pub struct SequenceFields<S: Sequencer> {
    sequencer: SequencerHandle<S>,
    sequence_id: u64,
    priority: Option<i32>,
    effective_priority: i32,
    token: Option<u64>,
}

impl<S: Sequencer> Default for SequenceFields<S> {
    fn default() -> Self {
        SequenceFields {
            sequencer: SequencerHandle::default(),
            sequence_id: 0,
            priority: None,
            effective_priority: 100,
            token: None,
        }
    }
}

/// The core of every sequence (UVM's `uvm_sequence_base`): starting it on a
/// sequencer, and sending items through it. `#[sequence]` implements it.
pub trait SequenceCore: Sequence + Sized {
    /// The sequencer the sequence runs on.
    type Sequencer: Sequencer;
    /// The sequence's type name, for arbiters and messages.
    const NAME: &'static str;

    #[doc(hidden)]
    fn __fields(&self) -> &SequenceFields<Self::Sequencer>;
    #[doc(hidden)]
    fn __fields_mut(&mut self) -> &mut SequenceFields<Self::Sequencer>;

    /// Sets the priority before the sequence starts; one left unset takes
    /// its parent's (`start_from`), else 100.
    fn set_priority(&mut self, priority: i32) {
        self.__fields_mut().priority = Some(priority);
    }

    /// The priority it runs with, once started.
    fn priority(&self) -> i32 {
        self.__fields().effective_priority
    }

    /// The id the sequencer gave it, once started.
    fn sequence_id(&self) -> u64 {
        self.__fields().sequence_id
    }

    /// Starts the sequence on `sequencer` and runs its body.
    async fn start(
        &mut self,
        sequencer: &SequencerHandle<Self::Sequencer>,
    ) -> Result<(), TestError> {
        self.__start(sequencer, None).await
    }

    /// Starts the sequence from `parent`, inheriting its priority unless
    /// one was set (UVM's `start(sequencer, parent)`).
    async fn start_from<P: SequenceCore>(
        &mut self,
        sequencer: &SequencerHandle<Self::Sequencer>,
        parent: &P,
    ) -> Result<(), TestError> {
        self.__start(sequencer, Some(parent.priority())).await
    }

    #[doc(hidden)]
    async fn __start(
        &mut self,
        sequencer: &SequencerHandle<Self::Sequencer>,
        parent_priority: Option<i32>,
    ) -> Result<(), TestError> {
        if !sequencer.is_bound() {
            return Err(TestError::new(format!(
                "`{}` is started on an unbound sequencer handle",
                Self::NAME
            )));
        }
        let fields = self.__fields_mut();
        fields.sequencer = sequencer.clone();
        fields.sequence_id = sequencer.core().new_sequence_id();
        fields.effective_priority = fields.priority.or(parent_priority).unwrap_or(100);
        // Its own seed source, as UVM reseeds a sequence when it starts.
        let source = seeds::sequence(Self::NAME, &sequencer.core().name());
        seeded(source, self.body()).await
    }

    /// The sequencer it runs on, typed (UVM's `p_sequencer`, with no cast).
    /// Not across an `.await`. Panics before the sequence is started.
    fn sequencer(&self) -> Ref<'_, Self::Sequencer> {
        self.__fields().sequencer.borrow()
    }

    /// The handle of the sequencer it runs on.
    fn sequencer_handle(&self) -> &SequencerHandle<Self::Sequencer> {
        &self.__fields().sequencer
    }

    /// The sequencer's configuration (its `#[context]`'s), shared: the
    /// context of an inline randomization,
    /// `randomize_with!(item, &self.config(), { .. })`.
    fn config(&self) -> Rc<<Self::Sequencer as HasContext>::Config>
    where
        Self::Sequencer: HasContext,
    {
        self.sequencer().context().shared_config()
    }

    /// Randomizes an item under the sequencer's configuration, the context
    /// its type declares (`#[randomize(context = C)]`).
    fn randomize_item<I>(&self, item: &mut I) -> Result<(), RandError>
    where
        I: Randomize,
        Self::Sequencer: HasContext<Config = I::Context>,
    {
        item.randomize_in(&self.config())
    }

    /// `randomize_item` with a constraint set for this call only.
    fn randomize_item_with<I, C>(&self, item: &mut I, set: C) -> Result<(), RandError>
    where
        I: Randomize,
        C: Constraints<I>,
        Self::Sequencer: HasContext<Config = I::Context>,
    {
        item.randomize_with_in(set, &self.config())
    }

    /// Waits until the sequencer grants the sequence the driver.
    async fn start_item(
        &mut self,
        _item: &mut <Self::Sequencer as Sequencer>::Item,
    ) -> Result<(), TestError> {
        let fields = self.__fields();
        if fields.token.is_some() {
            return Err(TestError::new(format!(
                "`{}`: `start_item` twice without `finish_item`",
                Self::NAME
            )));
        }
        let core = fields.sequencer.core().clone();
        let token = core
            .request(fields.sequence_id, Self::NAME, fields.effective_priority)
            .await;
        self.__fields_mut().token = Some(token);
        Ok(())
    }

    /// Sends the item to the driver and waits until it is done; returns the
    /// shared item, holding what the driver filled in.
    async fn finish_item(
        &mut self,
        item: <Self::Sequencer as Sequencer>::Item,
    ) -> Result<ItemHandle<<Self::Sequencer as Sequencer>::Item>, TestError> {
        let token = self.__fields_mut().token.take().ok_or_else(|| {
            TestError::new(format!(
                "`{}`: `finish_item` without `start_item`",
                Self::NAME
            ))
        })?;
        let fields = self.__fields();
        let core = fields.sequencer.core().clone();
        core.send(token, fields.sequence_id, item).await
    }

    /// The response the driver returned for `request`, waiting for it.
    async fn get_response(
        &mut self,
        request: &ItemHandle<<Self::Sequencer as Sequencer>::Item>,
    ) -> <Self::Sequencer as Sequencer>::Response {
        let core = self.__fields().sequencer.core().clone();
        core.response(request.transaction_id()).await
    }
}

/// A protocol's fields in a sequence (`#[sequence(base = ..)]`), for the
/// protocol's methods.
pub trait HasBase<B> {
    fn base(&self) -> &B;
    fn base_mut(&mut self) -> &mut B;
}
