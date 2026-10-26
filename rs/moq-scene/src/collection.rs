//! The scene graph, and the TOML file a [`Collection`] saves to.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};

use crate::Error;

/// Everything a studio composes: the canvas, the sources, and the scenes that
/// place them.
///
/// Saved with [`to_toml`](Self::to_toml) and loaded with
/// [`from_toml`](Self::from_toml). The file names devices, files, and broadcast
/// paths, never a relay URL or a token, so it is safe to commit and share.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
	/// The output every scene is drawn onto.
	pub canvas: Canvas,
	/// How a fade from preview to program behaves.
	#[serde(default)]
	pub transition: Transition,
	/// Every source, by its unique name. Scenes refer to sources by that name,
	/// so one camera can appear in many scenes while being opened once.
	#[serde(default)]
	pub sources: BTreeMap<String, Source>,
	/// Gain and mute of each audio-carrying source, by source name. A source
	/// without an entry plays at unity.
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub audio: BTreeMap<String, Audio>,
	/// The scenes, in the order the scene list shows them.
	#[serde(default)]
	pub scenes: Vec<Scene>,
}

impl Collection {
	/// The scene file format version this build reads and writes.
	///
	/// A file names its version, and loading any other one fails rather than
	/// guessing at fields whose meaning changed.
	pub const VERSION: u32 = 1;

	/// An empty collection with one blank scene on a 1080p, 30 fps canvas.
	pub fn new() -> Self {
		Self {
			canvas: Canvas::default(),
			transition: Transition::default(),
			sources: BTreeMap::new(),
			audio: BTreeMap::new(),
			scenes: vec![Scene::new("Scene")],
		}
	}

	/// Parse and validate a scene file.
	pub fn from_toml(text: &str) -> Result<Self, Error> {
		let mut table: toml::Table = toml::from_str(text).map_err(|err| Error::Format(err.to_string()))?;
		let version = match table.remove("version") {
			Some(toml::Value::Integer(version)) => version,
			Some(other) => return Err(Error::Format(format!("`version` must be an integer, got {other}"))),
			None => return Err(Error::Format("missing `version`".into())),
		};
		if version != i64::from(Self::VERSION) {
			return Err(Error::Version {
				found: version,
				supported: Self::VERSION,
			});
		}

		let collection = Self::deserialize(table).map_err(|err| Error::Format(err.to_string()))?;
		collection.validate()?;
		Ok(collection)
	}

	/// Serialize to the scene file format, `version` first.
	pub fn to_toml(&self) -> Result<String, Error> {
		self.validate()?;
		let body = toml::to_string(self).map_err(|err| Error::Format(err.to_string()))?;
		Ok(format!("version = {}\n\n{body}", Self::VERSION))
	}

	/// The scene called `name`.
	pub fn scene(&self, name: &str) -> Option<&Scene> {
		self.scenes.iter().find(|scene| scene.name == name)
	}

	/// The scene called `name`, to edit.
	pub fn scene_mut(&mut self, name: &str) -> Option<&mut Scene> {
		self.scenes.iter_mut().find(|scene| scene.name == name)
	}

	/// Check what the types cannot: the canvas can be encoded, names are unique,
	/// and every item and audio entry names a source that exists.
	pub fn validate(&self) -> Result<(), Error> {
		self.canvas.validate()?;

		let mut names = BTreeSet::new();
		for scene in &self.scenes {
			if scene.name.is_empty() {
				return Err(Error::Invalid("a scene has an empty name".into()));
			}
			if !names.insert(scene.name.as_str()) {
				return Err(Error::Invalid(format!("two scenes are named `{}`", scene.name)));
			}
			for item in &scene.items {
				if !self.sources.contains_key(&item.source) {
					return Err(Error::Invalid(format!(
						"scene `{}` places `{}`, which is not a source",
						scene.name, item.source
					)));
				}
				item.rect.validate(&scene.name, &item.source)?;
			}
		}

		if let Some(name) = self.sources.keys().find(|name| name.is_empty()) {
			return Err(Error::Invalid(format!("a source has an empty name: `{name}`")));
		}

		for (name, audio) in &self.audio {
			if !self.sources.contains_key(name) {
				return Err(Error::Invalid(format!(
					"audio settings for `{name}`, which is not a source"
				)));
			}
			if !audio.gain.is_finite() || audio.gain < 0.0 {
				return Err(Error::Invalid(format!("`{name}` has gain {}", audio.gain)));
			}
		}

		Ok(())
	}
}

impl Default for Collection {
	fn default() -> Self {
		Self::new()
	}
}

/// The output picture: its size and how often it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
	/// Width in pixels. Even, since the encoder subsamples chroma.
	pub width: u32,
	/// Height in pixels. Even, since the encoder subsamples chroma.
	pub height: u32,
	/// Pictures drawn and encoded per second.
	pub framerate: u32,
}

impl Canvas {
	/// The canvas size.
	pub fn size(&self) -> moq_video::Size {
		moq_video::Size::new(self.width, self.height)
	}

	/// How often a picture is drawn.
	pub fn framerate(&self) -> moq_video::FrameRate {
		moq_video::FrameRate::new(self.framerate, 1).expect("validated canvas framerate")
	}

	fn validate(&self) -> Result<(), Error> {
		if self.width == 0 || self.height == 0 || !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
			return Err(Error::Invalid(format!(
				"canvas {}x{} must be non-zero and even",
				self.width, self.height
			)));
		}
		moq_video::FrameRate::new(self.framerate, 1)
			.map_err(|err| Error::Invalid(format!("canvas framerate {}: {err}", self.framerate)))?;
		Ok(())
	}
}

impl Default for Canvas {
	fn default() -> Self {
		Self {
			width: 1920,
			height: 1080,
			framerate: 30,
		}
	}
}

/// How the program changes when the preview is faded in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
	/// How long a fade takes.
	#[serde(with = "humantime_serde")]
	pub fade: Duration,
}

impl Default for Transition {
	fn default() -> Self {
		Self {
			fade: Duration::from_millis(300),
		}
	}
}

/// One arrangement of sources on the canvas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
	/// Unique among the collection's scenes.
	pub name: String,
	/// What the scene draws, back to front.
	#[serde(default)]
	pub items: Vec<Item>,
}

impl Scene {
	/// An empty scene.
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			items: Vec::new(),
		}
	}
}

/// A source placed in a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
	/// The [`Collection::sources`] entry to draw.
	pub source: String,
	/// Where on the canvas, stretched to fill it.
	pub rect: Rect,
	/// Whether the item is drawn. A hidden item keeps its place.
	#[serde(default = "visible")]
	pub visible: bool,
}

fn visible() -> bool {
	true
}

/// A rectangle in canvas pixels, from the top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
	/// Left edge.
	pub x: f32,
	/// Top edge.
	pub y: f32,
	/// Width.
	pub width: f32,
	/// Height.
	pub height: f32,
}

impl Rect {
	/// The whole of `canvas`.
	pub fn fill(canvas: &Canvas) -> Self {
		Self {
			x: 0.0,
			y: 0.0,
			width: canvas.width as f32,
			height: canvas.height as f32,
		}
	}

	fn validate(&self, scene: &str, source: &str) -> Result<(), Error> {
		let finite = [self.x, self.y, self.width, self.height].iter().all(|v| v.is_finite());
		if !finite || self.width < 0.0 || self.height < 0.0 {
			return Err(Error::Invalid(format!(
				"scene `{scene}` places `{source}` at {self:?}, which is not a rectangle"
			)));
		}
		Ok(())
	}
}

/// Where a source's picture or sound comes from.
///
/// Device ids are the ones the matching `moq_video::capture` and
/// `moq_audio::capture` listings report; `None` means the system default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
	/// A camera.
	Camera {
		/// The camera's id, or the default camera.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		id: Option<String>,
	},
	/// A whole display.
	Display {
		/// The display's id, or the main display.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		id: Option<String>,
	},
	/// One window.
	Window {
		/// The window's id.
		id: String,
	},
	/// A microphone or other audio input.
	Microphone {
		/// The input's id, or the default input.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		id: Option<String>,
	},
	/// Everything the machine plays, where the platform can capture it.
	SystemAudio,
	/// A PNG or JPEG still.
	Image {
		/// The file, relative to the scene file's directory unless absolute.
		path: PathBuf,
	},
	/// A solid color.
	Color {
		/// The color, as `#rrggbb` or `#rrggbbaa`.
		color: Rgba,
	},
	/// A broadcast subscribed from the relay the studio is connected to.
	Broadcast {
		/// The broadcast path on that relay.
		path: String,
	},
}

impl Source {
	/// Whether the source has a picture to place in a scene.
	pub fn has_video(&self) -> bool {
		!matches!(self, Self::Microphone { .. } | Self::SystemAudio)
	}

	/// Whether the source feeds the program audio.
	pub fn has_audio(&self) -> bool {
		matches!(
			self,
			Self::Microphone { .. } | Self::SystemAudio | Self::Broadcast { .. }
		)
	}
}

/// The gain and mute of one source in the program audio.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
	/// Linear gain: 1.0 is unity.
	pub gain: f32,
	/// Whether the source is silenced, keeping its gain.
	#[serde(default)]
	pub muted: bool,
}

impl Default for Audio {
	fn default() -> Self {
		Self {
			gain: 1.0,
			muted: false,
		}
	}
}

/// An sRGB color with straight alpha, written `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, SerializeDisplay, DeserializeFromStr)]
pub struct Rgba(pub [u8; 4]);

impl fmt::Display for Rgba {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let [r, g, b, a] = self.0;
		if a == u8::MAX {
			write!(f, "#{r:02x}{g:02x}{b:02x}")
		} else {
			write!(f, "#{r:02x}{g:02x}{b:02x}{a:02x}")
		}
	}
}

impl FromStr for Rgba {
	type Err = Error;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		let invalid = || Error::Invalid(format!("`{text}` is not a #rrggbb or #rrggbbaa color"));
		let hex = text.strip_prefix('#').ok_or_else(invalid)?;
		if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
			return Err(invalid());
		}
		let mut rgba = [u8::MAX; 4];
		for (channel, pair) in rgba.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
			let pair = std::str::from_utf8(pair).map_err(|_| invalid())?;
			*channel = u8::from_str_radix(pair, 16).map_err(|_| invalid())?;
		}
		Ok(Self(rgba))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn example() -> Collection {
		let mut collection = Collection::new();
		collection.sources.insert(
			"camera".into(),
			Source::Camera {
				id: Some("/dev/video0".into()),
			},
		);
		collection.sources.insert(
			"logo".into(),
			Source::Image {
				path: "logo.png".into(),
			},
		);
		collection.sources.insert(
			"background".into(),
			Source::Color {
				color: Rgba([0x10, 0x20, 0x30, 0xff]),
			},
		);
		collection.sources.insert("mic".into(), Source::Microphone { id: None });
		collection.sources.insert(
			"guest".into(),
			Source::Broadcast {
				path: "studio/guest".into(),
			},
		);
		collection.audio.insert("mic".into(), Audio { gain: 0.5, muted: true });

		let canvas = collection.canvas;
		let scene = &mut collection.scenes[0];
		scene.items.push(Item {
			source: "background".into(),
			rect: Rect::fill(&canvas),
			visible: true,
		});
		scene.items.push(Item {
			source: "camera".into(),
			rect: Rect {
				x: 100.0,
				y: 50.0,
				width: 640.0,
				height: 360.0,
			},
			visible: false,
		});
		collection
	}

	#[test]
	fn round_trips() {
		let collection = example();
		let text = collection.to_toml().unwrap();
		assert!(text.starts_with("version = 1\n"), "{text}");
		assert_eq!(Collection::from_toml(&text).unwrap(), collection);
	}

	/// The file is meant to be read and diffed by people, so pin what it looks like.
	#[test]
	fn reads_a_handwritten_file() {
		let text = r##"
version = 1

[canvas]
width = 1280
height = 720
framerate = 60

[transition]
fade = "500ms"

[sources.cam]
kind = "camera"

[sources.bg]
kind = "color"
color = "#ff000080"

[audio.cam2]
gain = 2.0

[sources.cam2]
kind = "broadcast"
path = "room/alice"

[[scenes]]
name = "Main"
items = [
	{ source = "bg", rect = { x = 0, y = 0, width = 1280, height = 720 } },
	{ source = "cam", rect = { x = 10, y = 10, width = 320, height = 180 }, visible = false },
]
"##;
		let collection = Collection::from_toml(text).unwrap();
		assert_eq!(collection.canvas.framerate, 60);
		assert_eq!(collection.transition.fade, Duration::from_millis(500));
		assert_eq!(collection.sources["cam"], Source::Camera { id: None });
		assert_eq!(
			collection.sources["bg"],
			Source::Color {
				color: Rgba([255, 0, 0, 128])
			}
		);
		assert_eq!(
			collection.audio["cam2"],
			Audio {
				gain: 2.0,
				muted: false
			}
		);
		let main = collection.scene("Main").unwrap();
		assert!(main.items[0].visible);
		assert!(!main.items[1].visible);
	}

	#[test]
	fn refuses_another_version() {
		let text = example().to_toml().unwrap().replacen("version = 1", "version = 2", 1);
		let err = Collection::from_toml(&text).unwrap_err();
		assert!(matches!(err, Error::Version { found: 2, supported: 1 }), "{err}");

		let text = example().to_toml().unwrap().replacen("version = 1", "", 1);
		assert!(matches!(Collection::from_toml(&text), Err(Error::Format(_))));
	}

	/// A typo or a field from a newer build fails instead of being dropped on the
	/// next save.
	#[test]
	fn refuses_unknown_fields() {
		let text = example()
			.to_toml()
			.unwrap()
			.replacen("[canvas]", "[canvas]\ndepth = 3", 1);
		assert!(matches!(Collection::from_toml(&text), Err(Error::Format(_))));

		let text = example()
			.to_toml()
			.unwrap()
			.replacen("kind = \"camera\"", "kind = \"camera\"\nzoom = 2", 1);
		assert!(matches!(Collection::from_toml(&text), Err(Error::Format(_))));

		let text = example()
			.to_toml()
			.unwrap()
			.replacen("kind = \"camera\"", "kind = \"hologram\"", 1);
		assert!(matches!(Collection::from_toml(&text), Err(Error::Format(_))));
	}

	#[test]
	fn refuses_dangling_references() {
		let mut collection = example();
		collection.sources.remove("camera");
		assert!(matches!(collection.to_toml(), Err(Error::Invalid(_))));

		let mut collection = example();
		collection.audio.insert("nobody".into(), Audio::default());
		assert!(matches!(collection.validate(), Err(Error::Invalid(_))));
	}

	#[test]
	fn refuses_duplicate_scenes() {
		let mut collection = example();
		collection.scenes.push(Scene::new("Scene"));
		assert!(matches!(collection.validate(), Err(Error::Invalid(_))));
	}

	#[test]
	fn refuses_a_canvas_the_encoder_cannot_take() {
		for (width, height, framerate) in [(1921, 1080, 30), (0, 1080, 30), (1920, 1080, 0)] {
			let mut collection = example();
			collection.canvas = Canvas {
				width,
				height,
				framerate,
			};
			assert!(
				matches!(collection.validate(), Err(Error::Invalid(_))),
				"{width}x{height}@{framerate}"
			);
		}
	}

	#[test]
	fn refuses_a_degenerate_rect() {
		let mut collection = example();
		collection.scenes[0].items[0].rect.width = f32::NAN;
		assert!(matches!(collection.validate(), Err(Error::Invalid(_))));
	}

	#[test]
	fn colors_parse_and_print() {
		assert_eq!("#102030".parse::<Rgba>().unwrap(), Rgba([0x10, 0x20, 0x30, 0xff]));
		assert_eq!("#10203040".parse::<Rgba>().unwrap().to_string(), "#10203040");
		assert_eq!(Rgba([1, 2, 3, 255]).to_string(), "#010203");
		for bad in ["102030", "#1020", "#gg0000", "#ééé"] {
			assert!(bad.parse::<Rgba>().is_err(), "{bad}");
		}
	}
}
