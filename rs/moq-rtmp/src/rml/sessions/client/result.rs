use crate::rml::chunk_io::Packet;
use crate::rml::messages::MessagePayload;
use crate::rml::sessions::client::ClientSessionEvent;

/// A single result that is returned when the client session performs an action
/// or receives messages from the server.
#[derive(PartialEq, Debug)]
pub enum ClientSessionResult {
	/// A packet that is slated to be sent to the peer.  This packet should *ALWAYS* be sent
	/// in the order it produced and can only be dropped if it has explicitly been marked as
	/// able to be dropped.  Failing to do so may cause RTMP chunk deserialization errors on the
	/// other end due to RTMP chunk header compression.
	OutboundResponse(Packet),

	/// An event the client session is raising so consuming applications can perform custom logic
	RaisedEvent(ClientSessionEvent),

	/// The server session received a message that it could not handle.  This result
	/// allows the consumer application to do something with it if it wants to (special logging)
	UnhandleableMessageReceived(MessagePayload),
}
