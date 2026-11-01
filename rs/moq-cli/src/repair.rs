//! `moq repair`: finish a recording that `export mp4 --output` left fragmented.

use std::path::PathBuf;

use anyhow::Context;

/// Finish an `export mp4` recording that a crash or second signal left fragmented.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// The recording, finished in place as a regular MP4 that indexes every complete fragment.
	#[usage(arg, value_name = "FILE", value_hint = usage_rs::ValueHint::FilePath)]
	pub file: PathBuf,
}

impl Args {
	/// Index the recording and write its `moov`, or report that it is already finished.
	pub async fn run(self) -> anyhow::Result<()> {
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
