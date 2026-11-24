//! Tokens a [`Reconnect`](super::Reconnect) asks the caller for at each dial.

use std::{collections::VecDeque, task::Poll};

use url::Url;

use crate::auth::Token;

/// How the caller answered one [`Dial`].
pub(super) enum Answer<E> {
	Present(Vec<Token>),
	Fail(E),
}

/// What the policy holds of a dial it asked for: dropping it abandons the dial.
pub(super) type Pending<E> = kio::Consumer<Option<Answer<E>>>;

struct Queue<E> {
	waiting: VecDeque<Dial<E>>,
	closed: bool,
}

/// Tokens a reconnecting connection asks for at each dial, for a credential that must be fresh
/// when it is presented, such as a proof a peer admits once and only shortly after it was signed.
///
/// Attach one with [`Reconnect::with_dials`](super::Reconnect::with_dials) and answer each
/// [`next`](Self::next) dial: every dial waits for its answer, whatever starts it, a reconnect, a
/// GOAWAY's handover, a probe of the preferred transport, or the next address of a walk, so none
/// presents tokens signed for another. The wait counts against the dial's own deadline.
///
/// Answering is also how the policy retries: unlike fixed tokens, which end it at the first
/// failure before a session was established, it backs off and asks again, with the reason on
/// [`Dial::failure`], until a dial is [failed](Dial::fail).
///
/// Cheap to clone; every clone shares one queue, which one policy feeds: once that policy ends,
/// [`next`](Self::next) returns `None` and a policy given it later ends at its first dial.
pub struct Dials<E> {
	queue: kio::Producer<Queue<E>>,
}

impl<E> Clone for Dials<E> {
	fn clone(&self) -> Self {
		Self {
			queue: self.queue.clone(),
		}
	}
}

impl<E> Default for Dials<E> {
	fn default() -> Self {
		Self::new()
	}
}

impl<E> Dials<E> {
	/// An empty queue, which dials join once a policy is attached.
	pub fn new() -> Self {
		Self {
			queue: kio::Producer::new(Queue {
				waiting: VecDeque::new(),
				closed: false,
			}),
		}
	}

	/// The next dial waiting for its tokens, or `None` once the policy ended. A dial the policy
	/// abandoned meanwhile, past its deadline, is skipped.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<Dial<E>>> {
		let ready = |queue: &Queue<E>| match queue.closed || queue.waiting.iter().any(Dial::wanted) {
			true => Poll::Ready(()),
			false => Poll::Pending,
		};
		loop {
			let mut queue = match self.queue.poll(waiter, |queue| ready(queue)) {
				Poll::Ready(Ok(queue)) => queue,
				Poll::Ready(Err(_)) => return Poll::Ready(None),
				Poll::Pending => return Poll::Pending,
			};
			while let Some(dial) = queue.waiting.pop_front() {
				if dial.wanted() {
					return Poll::Ready(Some(dial));
				}
			}
			if queue.closed {
				return Poll::Ready(None);
			}
			// Every dial seen wanted was abandoned since: wait again rather than report an end.
		}
	}

	/// Ask for the tokens of a dial to `url`, after `failure`. Once closed, the dial goes
	/// unanswered at once, which ends the policy.
	pub(super) fn request(&self, url: Url, failure: Option<E>) -> Pending<E> {
		let answer = kio::Producer::new(None);
		let pending = answer.consume();
		if let Ok(mut queue) = self.queue.write()
			&& !queue.closed
		{
			queue.waiting.retain(Dial::wanted);
			queue.waiting.push_back(Dial { url, failure, answer });
		}
		pending
	}

	/// End [`next`](Self::next), dropping every dial still waiting.
	pub(super) fn close(&self) {
		if let Ok(mut queue) = self.queue.write() {
			queue.closed = true;
			queue.waiting.clear();
		}
	}
}

/// One dial of a reconnecting connection, waiting for the tokens it presents.
///
/// Dropping it unanswered fails it, as [`fail`](Self::fail) would.
#[must_use = "an unanswered dial ends the policy"]
pub struct Dial<E> {
	url: Url,
	failure: Option<E>,
	answer: kio::Producer<Option<Answer<E>>>,
}

impl<E> Dial<E> {
	/// Where the dial goes: one of the configured addresses, or where a GOAWAY redirected it.
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// Why the previous attempt or session ended, such as a refused credential, or `None` when
	/// nothing failed since a session was last established, as for the first dial or a GOAWAY's
	/// handover. Mirrors the connection's own record of why it is retrying.
	pub fn failure(&self) -> Option<&E> {
		self.failure.as_ref()
	}

	/// Present `tokens` on this dial, as [`Reconnect::with_tokens`](super::Reconnect::with_tokens)
	/// would: the first is the connection credential, and the rest ride AUTH on its session.
	pub fn present(self, tokens: Vec<Token>) {
		self.answer(Answer::Present(tokens));
	}

	/// End the policy with `err` instead of dialing, as for a [`failure`](Self::failure) no retry
	/// can repair. A probe of the preferred transport, which the serving session does without, is
	/// skipped instead.
	pub fn fail(self, err: E) {
		self.answer(Answer::Fail(err));
	}

	fn answer(self, answer: Answer<E>) {
		if let Ok(mut slot) = self.answer.write() {
			*slot = Some(answer);
		}
	}

	/// Whether the policy still waits for this dial's answer.
	fn wanted(&self) -> bool {
		self.answer.wanted()
	}
}

impl<E> std::fmt::Debug for Dial<E> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Dial").field("url", &self.url).finish_non_exhaustive()
	}
}
