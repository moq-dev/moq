use std::sync::Arc;

use crate::quic::driver::connection::ConnectionInner;

/// A send group on a [`Connection`](crate::quic::driver::connection::Connection)
///
/// Streams that [join](crate::quic::driver::SendStream::set_group) a group share bytes with
/// the connection's other groups: a higher priority group preempts every lower one, and groups
/// of equal priority get an equal share of bytes however many streams each has queued. A
/// stream's own priority orders it only against the group's other streams, and a datagram sent
/// in the group is ordered among them by its priority. Streams and datagrams outside any group
/// go before every group. See [`moq_quic::SendGroups`].
///
/// Clones share one group, released once the last clone is dropped and its streams finish
/// and its datagrams are sent. A group does not keep its connection open.
#[derive(Debug, Clone)]
pub struct SendGroup(Arc<Inner>);

/// How an outgoing stream or datagram is scheduled: its [`SendGroup`] and priority
pub type SendOptions<'a> = moq_quic::SendOptions<'a, SendGroup>;

/// The options as `conn` knows them.
///
/// # Panics
/// - when `options.group` belongs to another connection
pub(crate) fn to_quic<'a>(options: SendOptions<'a>, conn: &ConnectionInner) -> moq_quic::SendOptions<'a> {
	moq_quic::SendOptions {
		group: options.group.map(|group| group.id_on(conn)),
		priority: options.priority,
	}
}

#[derive(Debug)]
struct Inner {
	conn: Arc<ConnectionInner>,
	group: Option<moq_quic::SendGroup>,
}

impl SendGroup {
	pub(crate) fn new(conn: Arc<ConnectionInner>, priority: i64) -> Self {
		let group = conn.state.lock().inner.send_groups().open(priority);
		Self(Arc::new(Inner {
			conn,
			group: Some(group),
		}))
	}

	/// Change the group's priority, moving any data it has queued along with it
	pub fn set_priority(&self, priority: i64) {
		let mut conn = self.0.conn.state.lock();
		conn.inner.send_groups().set_priority(self.id(), priority);
	}

	/// Get the group's priority
	pub fn priority(&self) -> i64 {
		let mut conn = self.0.conn.state.lock();
		conn.inner.send_groups().priority(self.id())
	}

	/// The group as the connection knows it, which refuses it on any other connection.
	pub(crate) fn id(&self) -> &moq_quic::SendGroup {
		self.0.group.as_ref().expect("only taken on drop")
	}

	/// The group as `conn` knows it.
	///
	/// Call it before taking `conn`'s lock: moq-quic makes the same check, but panicking
	/// there poisons the lock, so every handle's drop panics again and aborts the process.
	///
	/// # Panics
	/// - when the group belongs to another connection
	pub(crate) fn id_on(&self, conn: &ConnectionInner) -> &moq_quic::SendGroup {
		assert!(
			std::ptr::eq(&*self.0.conn, conn),
			"send group used on another connection"
		);
		self.id()
	}
}

impl Drop for Inner {
	fn drop(&mut self) {
		if let Some(group) = self.group.take() {
			let mut conn = self.conn.state.lock();
			conn.inner.send_groups().close(group);
		}
	}
}
