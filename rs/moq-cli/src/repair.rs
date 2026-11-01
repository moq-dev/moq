//! `moq repair <format>`: finish a recording that its recorder left unfinished.

use std::path::PathBuf;

use anyhow::Context;

/// Finish a recording that a crash or second signal left unfinished.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	#[usage(subcommand)]
	format: Format,
}

impl Args {
	/// Repair the recording in the named format.
	pub async fn run(self) -> anyhow::Result<()> {
		match self.format {
			Format::Mp4(mp4) => mp4.run().await,
		}
	}
}

/// The recorder that left the file, one per format that can be cut short.
#[derive(usage_rs::Subcommands, Clone)]
enum Format {
	/// Finish an `export mp4` recording that a crash or second signal left fragmented.
	Mp4(Mp4),
}

/// `moq repair mp4 <FILE>`.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
struct Mp4 {
	/// The recording, finished in place as a regular MP4 that indexes every complete fragment.
	#[usage(arg, value_name = "FILE", value_hint = usage_rs::ValueHint::FilePath)]
	file: PathBuf,
}

impl Mp4 {
	/// Index the recording and write its `moov`, or report that it is already finished.
	async fn run(self) -> anyhow::Result<()> {
		let path = self.file;
		let file = std::fs::OpenOptions::new()
			.read(true)
			.write(true)
			.open(&path)
			.with_context(|| format!("failed to open {}", path.display()))?;
		let (file, finish) = tokio::task::spawn_blocking(move || {
			let mut file = file;
			let finish = moq_mux::container::mp4::repair(&mut file);
			(file, finish)
		})
		.await?;
		let finish = finish.with_context(|| format!("failed to repair {}", path.display()))?;

		match finish {
			Some(finish) => {
				crate::subscribe::write_finish(&mut tokio::fs::File::from_std(file), &finish).await?;
				tracing::info!(path = %path.display(), "recording repaired");
			}
			None => tracing::info!(path = %path.display(), "recording already finished"),
		}
		Ok(())
	}
}
