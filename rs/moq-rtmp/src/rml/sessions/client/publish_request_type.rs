/// The type of publish request being made
pub enum PublishRequestType {
	/// The published stream should be sent out without recording it in a file
	Live,

	/// The published stream should be recorded to a new file (RTMP spec says
	/// the file should be overwritten if it already exists).
	Record,

	/// The stream is published and the data should be appended to a file
	Append,
}
