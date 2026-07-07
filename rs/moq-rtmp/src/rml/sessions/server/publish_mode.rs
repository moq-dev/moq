/// The type of publishing being performed or requested
#[derive(Eq, PartialEq, Clone, Debug)]
pub enum PublishMode {
	/// Live data is being published without recording it in a file
	Live,

	/// The stream is intended to be published to a file
	Record,

	/// The stream is published and the data is intended to a file
	Append,
}
