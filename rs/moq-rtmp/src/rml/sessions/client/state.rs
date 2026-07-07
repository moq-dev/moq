#[derive(Clone, Debug)]
pub enum ClientState {
	/// Client has not connected to an application on the server yet,
	Disconnected,

	/// The client has connected to an application on the server
	Connected,

	/// Playback has been requested for a stream key and we are still waiting for a response
	PlayRequested,

	/// We are currently playing back a stream from the server
	Playing,

	/// Publish has been requested and we are waiting for a response
	PublishRequested,

	/// We are currently publishing to the server
	Publishing,
}
