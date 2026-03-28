#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("mp4: {0}")]
	Mp4(#[from] mp4_atom::Error),

	#[error("moq: {0}")]
	Moq(#[from] moq_lite::Error),

	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_lite::TimeOverflow),

	#[error("no traf in moof")]
	NoTraf,

	#[error("no tfdt in traf")]
	NoTfdt,

	#[error("no moof found in CMAF frame data")]
	NoMoof,

	#[error("no mdat found in CMAF frame data")]
	NoMdat,

	#[error("no moov found in init data")]
	NoMoov,

	#[error("no tracks in moov")]
	NoTracks,

	#[error("multiple tracks in moov, use Trak instead")]
	MultipleTracks,
}
