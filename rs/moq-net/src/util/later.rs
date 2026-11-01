use std::task::Poll;

use crate::Error;

/// What one request learns for the requests sent alongside it, which wait on it only once
/// they need it: a track's info, say, that a fetch's answer is read in.
pub(crate) struct Learning<T>(kio::Producer<Option<Result<T, Error>>>);

impl<T: Clone> Learning<T> {
	pub fn new() -> Self {
		Self(kio::Producer::new(None))
	}

	/// A handle to wait on the answer.
	pub fn later(&self) -> Later<T> {
		Later::Pending(self.0.consume())
	}

	/// Answer every waiter; only the first answer counts.
	pub fn resolve(&self, result: Result<T, Error>) {
		if let Ok(mut learned) = self.0.write()
			&& learned.is_none()
		{
			*learned = Some(result);
		}
	}
}

/// A value already known, or one a [`Learning`] still owes.
#[derive(Clone)]
pub(crate) enum Later<T> {
	Known(T),
	Pending(kio::Consumer<Option<Result<T, Error>>>),
}

impl<T: Clone> Later<T> {
	/// The answer, if it came.
	pub fn peek(&self) -> Option<Result<T, Error>> {
		match self {
			Self::Known(value) => Some(Ok(value.clone())),
			Self::Pending(learned) => learned.read().clone(),
		}
	}

	pub fn poll(&self, waiter: &kio::Waiter) -> Poll<Result<T, Error>> {
		let learned = match self {
			Self::Known(value) => return Poll::Ready(Ok(value.clone())),
			Self::Pending(learned) => learned,
		};
		match learned.poll(waiter, |learned| match &**learned {
			Some(result) => Poll::Ready(result.clone()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(result)) => Poll::Ready(result),
			// Gone unanswered: the request it waited on was abandoned.
			Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Cancel)),
			Poll::Pending => Poll::Pending,
		}
	}
}
