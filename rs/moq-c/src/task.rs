use std::future::Future;

use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State};

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// `close` is an `Option` so closing can drop just the sender without removing the
/// entry or revoking the callback. The task delivers one final terminal callback and
/// then removes itself, so `user_data` stays valid until that callback fires.
pub(crate) struct Task {
	close: Option<oneshot::Sender<()>>,
	pub callback: OnStatus,
}

impl Task {
	/// An entry, plus the receiver its task watches for [`Self::close`].
	pub fn new(callback: OnStatus) -> (Self, oneshot::Receiver<()>) {
		let (close, closed) = oneshot::channel();
		let task = Self {
			close: Some(close),
			callback,
		};
		(task, closed)
	}

	/// Signal shutdown, erroring with `missing` if it was already signalled.
	pub fn close(&mut self, missing: Error) -> Result<(), Error> {
		self.close.take().map(drop).ok_or(missing)
	}
}

impl NonZeroSlab<Task> {
	/// Spawn `run` with its close receiver, then deliver its result as the terminal callback.
	///
	/// `slab` finds this slab again from the task, so it can remove its own entry.
	pub fn spawn<F>(
		&mut self,
		callback: OnStatus,
		slab: fn(&mut State) -> &mut Self,
		run: impl FnOnce(oneshot::Receiver<()>) -> F,
	) -> Result<Id, Error>
	where
		F: Future<Output = Result<(), Error>> + Send + 'static,
	{
		let (task, closed) = Task::new(callback);
		let id = self.insert(task)?;
		let run = run(closed);

		tokio::spawn(async move {
			let res = crate::ffi::run(run).await;

			// Pull the entry out from under the lock so the callback never runs while held.
			let entry = slab(&mut State::lock()).remove(id);
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	/// Signal a task to shut down; it still delivers its terminal callback.
	///
	/// Errors with `missing` for an unknown id or one already closed.
	pub fn close(&mut self, id: Id, missing: Error) -> Result<(), Error> {
		self.get_mut(id).and_then(|task| task.close.take()).ok_or(missing)?;
		Ok(())
	}
}
