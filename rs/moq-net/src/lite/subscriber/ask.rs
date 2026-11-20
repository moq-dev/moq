//! A finished lite-07 copy asking its upstream where its readers' relative starts begin.

use std::task::{Poll, ready};

use super::{Accepted, Establish, SubStream, TrackServe, wire_ranges};
use crate::{
	Error,
	coding::Recv,
	lite,
	track::{self, Order},
};

/// A SUBSCRIBE of its own for the relative ranges a finished copy's readers wait on their
/// publisher to start (see [`track::Producer::asks`]): the subscription that fed the copy is
/// over, so nothing else can carry them, and the copy never counts them over the groups it
/// holds. Its SUBSCRIBE_START answers them, and the groups it delivers land in the copy, so a
/// reader gets what a cold copy would. It FINs once the publisher declared the end and every
/// group from the lowest start up to it is accounted for and read, then waits for the
/// publisher's FIN in reply.
pub(super) struct Ask {
	serve: TrackServe,
	serving: track::Producer,
	id: u64,
	state: AskState,
}

enum AskState {
	Establish(Box<Establish>),
	Active {
		sub: Box<SubStream>,
		/// Whether our FIN went out.
		finished: bool,
	},
	Done,
}

impl Ask {
	/// Ask for what `serving`'s readers wait on, `None` when they wait on nothing.
	pub fn new(serve: &TrackServe, serving: &track::Producer) -> Option<Self> {
		let asks = serving.asks();
		if asks.is_empty() {
			return None;
		}
		// Under the readers' own preferences, in key order, as a cold copy's reader asks.
		let subscription = serving
			.subscription()
			.unwrap_or_default()
			.with_ranges(asks)
			.with_order(Order::Ascending);
		let mut ranges = wire_ranges(&subscription)?;
		// A relative end goes upstream open, so two asks may carry the same range.
		let mut seen = std::collections::HashSet::new();
		ranges.retain(|range| seen.insert(*range));
		// The asks that don't fit wait for those sent to be answered, which changes the asks.
		let fields = serve.subscribe_fields(&subscription);
		let ranges = super::fit::Budget::new(serve.subscriber.version, fields, 0, ranges.len()).take(ranges);
		if ranges.is_empty() {
			return None;
		}
		let copy = Accepted {
			producer: serving.clone(),
			framing: lite::Framing::new(serving.info(), serve.subscriber.version),
		};
		tracing::debug!(track = %serve.name, ?ranges, "asking where relative starts begin");
		let establish = serve.register(subscription, ranges, Some(copy));
		Some(Self {
			serve: serve.clone(),
			serving: serving.clone(),
			id: establish.id,
			state: AskState::Establish(Box::new(establish)),
		})
	}

	/// Ready once the ask is over, or with why it failed: its readers then have no answer
	/// coming.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				AskState::Establish(est) => match ready!(est.poll(waiter)) {
					Ok(active) => {
						self.state = AskState::Active {
							sub: Box::new(active),
							finished: false,
						}
					}
					Err(err) => {
						self.end();
						return Poll::Ready(Err(err));
					}
				},
				AskState::Active { sub, finished } => {
					if !*finished && let (Some(started), Some(end)) = (sub.started, &sub.end) {
						let owed = started.unwrap_or(end.group)..end.group;
						let settled = sub
							.tail
							.consume()
							.poll(waiter, |tail| match tail.covers(owed.clone()) && !tail.is_reading() {
								true => Poll::Ready(()),
								false => Poll::Pending,
							})
							.is_ready();
						if settled {
							let _ = sub.stream.writer.finish();
							*finished = true;
						}
					}
					if let Poll::Ready(Err(err)) = sub.stream.writer.poll_flush(waiter) {
						tracing::debug!(%err, "ask subscribe write failed");
					}
					let res = ready!(sub.stream.reader.poll_recv::<lite::SubscribeResponse>(waiter));
					let now = self.serve.subscriber.runtime.now();
					match res {
						Recv::Message(lite::SubscribeResponse::Start(start))
							if sub.started.is_none() && start.starts.len() as u64 == sub.live.initial_count() =>
						{
							let edge = start.largest.map(|largest| largest.position.group);
							for (id, start) in (0..).zip(&start.starts) {
								sub.starts.insert(id, *start);
								sub.edges.insert(id, edge);
							}
							sub.started = Some(start.starts.iter().flatten().min().copied());
							sub.report_starts(&self.serving);
						}
						Recv::Message(lite::SubscribeResponse::Drop(dropped)) => {
							if let Ok(mut tail) = sub.tail.write() {
								tail.account(dropped.groups, now);
							}
						}
						// Never produced: the copy records the skip, as the subscription itself does.
						Recv::Message(lite::SubscribeResponse::Skip(skipped)) => {
							if !skipped.groups.is_empty() {
								self.serving.skip_received(skipped.groups.clone());
								if let Ok(mut tail) = sub.tail.write() {
									tail.account(skipped.groups, now);
								}
							}
						}
						Recv::Message(lite::SubscribeResponse::Finish(finish)) if sub.end.is_none() => {
							sub.end = Some(finish);
						}
						Recv::Message(lite::SubscribeResponse::Ok(_)) => {}
						// A second START or FINISH, or a STARTED for a range no update added.
						Recv::Message(msg) => {
							tracing::warn!(?msg, "unexpected ask subscribe response");
							self.end();
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
						// The publisher FINs in reply to ours, or once it finished the track.
						Recv::Ended if *finished || sub.end.is_some() => {
							self.end();
							return Poll::Ready(Ok(()));
						}
						Recv::Ended | Recv::Malformed(_) => {
							tracing::warn!("ask subscribe ended without SUBSCRIBE_FINISH");
							self.end();
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
						// After our FIN everything it owes is in, so a reset, such as the publisher
						// cutting a response short, costs nothing.
						Recv::Reset(err) => {
							tracing::debug!(%err, "ask subscribe ended");
							let finished = *finished;
							self.end();
							return Poll::Ready(if finished { Ok(()) } else { Err(err) });
						}
					}
				}
				AskState::Done => return Poll::Ready(Ok(())),
			}
		}
	}

	/// Stop routing the subscription's streams, resetting it unless our FIN already went out.
	fn end(&mut self) {
		let state = std::mem::replace(&mut self.state, AskState::Done);
		self.serve.subscriber.remove_subscribe(self.id);
		match state {
			AskState::Establish(est) => est.abort(&Error::Cancel),
			AskState::Active { sub, finished: false } => sub.stream.writer.abort(&Error::Cancel),
			_ => {}
		}
	}
}

impl Drop for Ask {
	fn drop(&mut self) {
		if !matches!(self.state, AskState::Done) {
			self.end();
		}
	}
}
