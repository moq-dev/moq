//! What a session presents to its peer, and what the peer lets it do.

use std::ffi::c_void;

use tokio::sync::oneshot;

use crate::ffi::{self, OnStatus};
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, moq_string};

/// Grant snapshots and the watchers delivering them.
#[derive(Default)]
pub struct Auth {
	grant: NonZeroSlab<GrantRecord>,
	grants: NonZeroSlab<Task>,
}

/// One grant delivered to a [moq_session_grants] callback, owning the strings it lends.
struct GrantRecord {
	grant: Option<moq_net::auth::Grant>,
	/// Own the strings the views borrow; never read directly.
	_publish: Vec<String>,
	_subscribe: Vec<String>,
	publish_views: Vec<moq_string>,
	subscribe_views: Vec<moq_string>,
}

// SAFETY: the views point into `publish` and `subscribe`, owned by the same record and never
// mutated after construction, so moving the record between threads keeps them valid.
unsafe impl Send for GrantRecord {}

impl GrantRecord {
	fn new(grant: Option<moq_net::auth::Grant>) -> Self {
		let patterns = |patterns: &moq_net::Patterns| patterns.iter().map(ToString::to_string).collect::<Vec<_>>();
		let publish = grant.as_ref().map(|grant| patterns(&grant.publish)).unwrap_or_default();
		let subscribe = grant.as_ref().map(|grant| patterns(&grant.subscribe)).unwrap_or_default();
		let views = |strings: &[String]| {
			strings
				.iter()
				.map(|s| moq_string {
					data: s.as_ptr().cast(),
					len: s.len(),
				})
				.collect()
		};
		Self {
			publish_views: views(&publish),
			subscribe_views: views(&subscribe),
			grant,
			_publish: publish,
			_subscribe: subscribe,
		}
	}
}

/// What the peer lets this side do, in this side's own paths.
///
/// An empty pattern list grants nothing in that direction; `**` grants everything.
#[repr(C)]
pub struct moq_grant {
	/// False until the peer first answers a token, and while a client reconnects; the other
	/// fields are then empty.
	pub present: bool,
	/// The broadcast patterns this side may publish to the peer.
	pub publish: *const moq_string,
	/// The number of entries in `publish`.
	pub publish_len: usize,
	/// The broadcast patterns this side may subscribe to from the peer.
	pub subscribe: *const moq_string,
	/// The number of entries in `subscribe`.
	pub subscribe_len: usize,
	/// Microseconds until the grant lapses, measured when this was read. Meaningless unless
	/// `has_expires`.
	pub expires_us: u64,
	/// Whether the grant lapses at all.
	pub has_expires: bool,
}

impl Auth {
	fn grants(&mut self, handle: moq_net::auth::Handle, callback: OnStatus) -> Result<Id, Error> {
		let watch = handle.grant();
		self.grants.spawn(
			callback,
			|state| &mut state.auth.grants,
			|close| Self::run_grants(callback, watch, close),
		)
	}

	async fn run_grants(
		callback: OnStatus,
		mut watch: moq_net::auth::Watch,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		let mut grant = watch.peek();
		loop {
			let id = State::lock().auth.grant.insert(GrantRecord::new(grant))?;
			callback.call(id);
			// `biased` so a pending close always wins over a ready change.
			grant = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				changed = kio::wait(|waiter| watch.poll_changed(waiter)) => match changed {
					Ok(grant) => grant,
					// Nothing will change the grant again: the session ended.
					Err(_) => return Ok(()),
				},
			};
		}
	}

	fn grant_info(&self, id: Id, dst: &mut moq_grant) -> Result<(), Error> {
		let record = self.grant.get(id).ok_or(Error::NotFound)?;
		let expires = record.grant.as_ref().and_then(|grant| grant.expires);
		let remaining = expires.map(|expires| expires.saturating_duration_since(moq_net::time::Instant::now()));
		*dst = moq_grant {
			present: record.grant.is_some(),
			publish: record.publish_views.as_ptr(),
			publish_len: record.publish_views.len(),
			subscribe: record.subscribe_views.as_ptr(),
			subscribe_len: record.subscribe_views.len(),
			expires_us: remaining.map_or(0, |remaining| remaining.as_micros().try_into().unwrap_or(u64::MAX)),
			has_expires: remaining.is_some(),
		};
		Ok(())
	}
}

/// Replace the tokens a dialed session presents, such as before the current ones expire.
///
/// The first is the connection credential, unless the URL carries its own `?jwt=`; each
/// other one widens the grant over in-band AUTH. The next dial presents them, and a live
/// session that supports in-band AUTH presents them at once, withdrawing each replaced
/// token once its replacements are answered, even when one is refused. After an
/// authorization refusal the session redials only if they changed. Wait for the peer's
/// verdict with [moq_session_tokens_answered].
///
/// Returns zero on success, or a negative code: `MOQ_ERROR_UNSUPPORTED` for a session a
/// server accepted, which presents no tokens, and `MOQ_ERROR_INVALID_CONFIG` for an empty token.
///
/// # Safety
/// - `tokens` must point at `tokens_len` valid [moq_string]s, or be NULL with a zero length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_tokens_set(session: u32, tokens: *const moq_string, tokens_len: usize) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let tokens = unsafe { ffi::parse_strings(tokens, tokens_len)? }
			.into_iter()
			.map(|token| {
				token
					.try_into()
					.map_err(|err: moq_net::auth::EmptyToken| Error::InvalidConfig(err.to_string()))
			})
			.collect::<Result<Vec<moq_net::auth::Token>, Error>>()?;
		State::lock().session.tokens(session)?.set(tokens);
		Ok(())
	})
}

/// Wait for the peer's first verdict on the tokens the last [moq_session_tokens_set] newly
/// presented, or on the dialed ones before any set.
///
/// `on_answer` fires exactly once: `0` once each token is granted, in band or as a later
/// dial's connection credential, or a negative code. A refusal carries the peer's code
/// through [crate::moq_error_protocol]; `MOQ_ERROR_UNSUPPORTED` means the session cannot carry
/// an in-band token; the connection's end, including [crate::moq_session_close], is its close
/// reason. A token the set kept counts only once refused, and nothing is re-checked after the
/// answer.
///
/// Returns zero once the wait started, or a negative code, `MOQ_ERROR_UNSUPPORTED` for a
/// session a server accepted, in which case `on_answer` never fires.
///
/// # Safety
/// - `on_answer` must be non-NULL, and `user_data` valid until it fires.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_tokens_answered(
	session: u32,
	on_answer: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let callback = unsafe { OnStatus::new(user_data, on_answer)? };
		let tokens = State::lock().session.tokens(session)?;
		tokio::spawn(async move {
			let res = ffi::run(async { Ok(kio::wait(|waiter| tokens.poll_answered(waiter)).await?) }).await;
			callback.call(res);
		});
		Ok(())
	})
}

/// Follow the grant a session's peer gave it: the union of every token the session presented.
///
/// `on_grant` fires at once with a positive grant handle for the current grant, again with a
/// new handle on every change, then exactly once more with a terminal `0` once nothing will
/// change it (the session ended, or [moq_session_grants_cancel]) or a negative error; release
/// `user_data` there. Read each handle with [moq_grant_info] and release it with
/// [moq_grant_free]. A dialed session's grant survives reconnects, reading not present while
/// offline. AUTH is only on `moq-lite-07-wip` and on moq-transport sessions that negotiate the
/// MoQ Auth extension; elsewhere the grant stays not present.
///
/// Returns a positive watcher handle, or a negative code: `MOQ_ERROR_OFFLINE` for an accepted
/// session still in SETUP.
///
/// # Safety
/// - `on_grant` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_grants(
	session: u32,
	on_grant: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let callback = unsafe { OnStatus::new(user_data, on_grant)? };
		let mut state = State::lock();
		let handle = state.session.auth(session)?;
		state.auth.grants(handle, callback)
	})
}

/// Stop a grant watcher from [moq_session_grants].
///
/// Its callback still fires once more with a terminal `0`; release `user_data` there. Grant
/// handles already delivered stay valid until freed. Returns zero, or a negative code if it
/// already ended.
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_grants_cancel(watcher: u32) -> i32 {
	ffi::enter(move || {
		let watcher = ffi::parse_id(watcher)?;
		State::lock().auth.grants.close(watcher, Error::NotFound)
	})
}

/// Read a grant delivered to a [moq_session_grants] callback.
///
/// The pattern strings borrow the grant handle's storage until [moq_grant_free].
/// Returns zero on success, or a negative code on failure.
///
/// # Safety
/// - `dst` must point at a writable [moq_grant].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_grant_info(grant: u32, dst: *mut moq_grant) -> i32 {
	ffi::enter(move || {
		let grant = ffi::parse_id(grant)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().auth.grant_info(grant, dst)
	})
}

/// Release a grant handle delivered to a [moq_session_grants] callback.
#[unsafe(no_mangle)]
pub extern "C" fn moq_grant_free(grant: u32) -> i32 {
	ffi::enter(move || {
		let grant = ffi::parse_id(grant)?;
		State::lock().auth.grant.remove(grant).ok_or(Error::NotFound)?;
		Ok(())
	})
}
