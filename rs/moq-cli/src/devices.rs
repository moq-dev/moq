//! `moq devices`: list what the capture flags can name.
//!
//! Each section prints the identifier `moq import capture` expects in the first
//! column, so the output can be read and pasted straight into `--camera`,
//! `--display`, `--window`, `--app`, or `--microphone`.

use std::fmt::Write;

/// Print every capture source this platform can enumerate.
///
/// A source that this platform doesn't implement is reported inline rather than
/// failing the whole listing: a Linux box with no window enumeration should
/// still get its cameras.
pub async fn run() -> anyhow::Result<()> {
	let mut out = String::new();

	section(
		&mut out,
		"Cameras",
		moq_video::capture::cameras().await,
		|out, cameras| {
			for camera in cameras {
				writeln!(out, "  {}  {}", camera.id, camera.name).unwrap();
			}
		},
	);

	section(
		&mut out,
		"Displays",
		moq_video::capture::displays().await,
		|out, displays| {
			for display in displays {
				writeln!(
					out,
					"  {}  {} ({}x{})",
					display.id, display.name, display.width, display.height
				)
				.unwrap();
			}
		},
	);

	section(
		&mut out,
		"Windows",
		moq_video::capture::windows().await,
		|out, windows| {
			for window in windows {
				let title = if window.title.is_empty() {
					"(untitled)"
				} else {
					&window.title
				};
				writeln!(
					out,
					"  {}  {} - {} ({}x{})",
					window.id, window.app, title, window.width, window.height
				)
				.unwrap();
			}
		},
	);

	section(
		&mut out,
		"Applications",
		moq_video::capture::apps().await,
		|out, apps| {
			for app in apps {
				writeln!(out, "  {}  {}", app.id, app.name).unwrap();
			}
		},
	);

	section(
		&mut out,
		"Microphones",
		moq_audio::capture::devices().await,
		|out, devices| {
			for device in devices {
				// The default is what `--microphone` picks when omitted, so mark it.
				let marker = if device.default { "*" } else { " " };
				// cpal has no id beyond the name, so printing both would repeat
				// itself; the id is the column `--microphone` takes.
				writeln!(out, "  {marker} {}", device.id).unwrap();
			}
		},
	);

	print!("{out}");
	Ok(())
}

/// Render one section: its items, the reason it's empty, or why this platform
/// can't list it.
fn section<T, E: std::fmt::Display>(
	out: &mut String,
	title: &str,
	result: Result<Vec<T>, E>,
	render: impl FnOnce(&mut String, &[T]),
) {
	writeln!(out, "{title}:").unwrap();
	match result {
		Ok(items) if items.is_empty() => writeln!(out, "  (none)").unwrap(),
		Ok(items) => render(out, &items),
		Err(err) => writeln!(out, "  ({err})").unwrap(),
	}
	writeln!(out).unwrap();
}
