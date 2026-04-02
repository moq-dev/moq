/// Errors from moq-mux operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("moq: {0}")]
	Moq(#[from] moq_lite::Error),

	#[error("hang: {0}")]
	Hang(#[from] hang::Error),

	#[cfg(feature = "mp4")]
	#[error("cmaf: {0}")]
	Cmaf(#[from] crate::cmaf::Error),
}
