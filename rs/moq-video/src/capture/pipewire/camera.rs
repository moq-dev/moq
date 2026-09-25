//! PipeWire camera capture: the `Video/Source` nodes with the `Camera` role,
//! which is how PipeWire presents V4L2 webcams (spa-v4l2) and libcamera sensors
//! such as a Raspberry Pi CSI camera (spa-libcamera). A CSI camera has no V4L2
//! node that yields processed frames, so on a Pi this is the only way to open it
//! as a camera.
//!
//! Outside a sandbox this connects to the session's PipeWire socket, which also
//! works headless. A sandbox cannot reach that socket, so there the camera portal
//! (`org.freedesktop.portal.Camera`) grants access and hands back a PipeWire
//! remote that exposes the cameras. Streaming reuses the parent module's loop.
//!
//! The mode is chosen here rather than negotiated: the node's `EnumFormat` list
//! goes through the same selection as a V4L2 device, and the stream offers
//! exactly the chosen mode. Offering a size range instead lets spa-v4l2 accept
//! a size its format does not have (a webcam with YUY2 only at 640x480 took
//! YUY2 at 1280x720) and then fail the link.

use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Duration;

use pipewire as pw;
use pw::spa;
use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use spa::param::video::{VideoFormat, VideoInfoRaw};
use spa::pod::{ChoiceValue, Value};
use spa::utils::ChoiceEnum;

use super::{Capture, DEFAULT_FRAMERATE, Kind, Target, drm_format, err, linear_modifier, serialize_format};
use crate::capture::mode::{self, Request};
use crate::capture::v4l2::{DEFAULT_HEIGHT, DEFAULT_WIDTH, bounds};
use crate::capture::{Camera, Config, Mode, PIPEWIRE, Stream};
use crate::{Error, Rate, Size};

/// How long the PipeWire daemon may take to answer a query. A connected daemon
/// answers in milliseconds, so running out means it is stuck.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// List the PipeWire cameras as [`Camera`]s with `pipewire:<node name>` ids.
pub(in crate::capture) async fn cameras() -> Result<Vec<Camera>, Error> {
	let sandboxed = ashpd::is_sandboxed();
	let remote = if sandboxed {
		match portal().await? {
			Some(fd) => Some(fd),
			// No camera lists as nothing, so the V4L2 cameras still show.
			None => return Ok(Vec::new()),
		}
	} else {
		None
	};
	let nodes = crate::capture::blocking(move || {
		pw::init();
		let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| err("pipewire main loop", e))?;
		let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| err("pipewire context", e))?;
		let core = match super::connect(&context, remote) {
			Ok(core) => core,
			// PipeWire is optional on the host, and a host without it has no
			// PipeWire cameras. A portal remote that fails to connect is broken.
			Err(error) if !sandboxed => {
				tracing::debug!(%error, "no PipeWire session to list cameras from");
				return Ok(Vec::new());
			}
			Err(error) => return Err(err("pipewire connect", error)),
		};
		let registry = core.get_registry_rc().map_err(|e| err("pipewire registry", e))?;
		scan(&mainloop, &core, &registry)
	})
	.await?;

	Ok(nodes
		.into_iter()
		.map(|node| Camera {
			id: format!("{PIPEWIRE}:{}", node.name),
			name: node.description,
		})
		.collect())
}

/// List the modes the camera named `node` (or the default camera) reports, for
/// the formats this backend converts.
pub(in crate::capture) async fn modes(node: Option<&str>) -> Result<Vec<Mode>, Error> {
	let remote = remote().await?;
	let node = node.map(str::to_string);
	crate::capture::blocking(move || {
		pw::init();
		let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| err("pipewire main loop", e))?;
		let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| err("pipewire context", e))?;
		let core = super::connect(&context, remote).map_err(|e| err("pipewire connect", e))?;
		let registry = core.get_registry_rc().map_err(|e| err("pipewire registry", e))?;
		let nodes = scan(&mainloop, &core, &registry)?;
		let node = resolve(&nodes, node.as_deref())?;
		let formats = formats(&mainloop, &core, &registry, node)?;
		Ok(mode::modes(formats.into_iter().flat_map(|format| {
			format.sizes.into_iter().map(move |size| (size, format.rates.clone()))
		})))
	})
	.await
}

/// Open the camera node named `node`, or the default camera, in the mode
/// nearest `config`'s size and rate.
pub(in crate::capture) async fn open(config: &Config, node: Option<&str>) -> Result<Stream, Error> {
	let remote = remote().await?;
	let label = match node {
		Some(node) => format!("{PIPEWIRE}:{node}"),
		None => PIPEWIRE.to_string(),
	};
	let want = Request {
		size: Size::new(
			config.width.unwrap_or(DEFAULT_WIDTH),
			config.height.unwrap_or(DEFAULT_HEIGHT),
		),
		framerate: config.framerate,
	};
	super::start(
		config,
		Capture {
			kind: Kind::Camera,
			remote,
			target: Target::Camera(node.map(str::to_string), want),
			label,
		},
		None,
	)
	.await
}

/// The camera portal's PipeWire remote inside a sandbox, or `None` to use the
/// session's socket.
async fn remote() -> Result<Option<OwnedFd>, Error> {
	if !ashpd::is_sandboxed() {
		return Ok(None);
	}
	match portal().await? {
		Some(fd) => Ok(Some(fd)),
		None => Err(Error::SourceUnavailable(
			"the camera portal reports no camera".to_string(),
		)),
	}
}

/// Ask the camera portal for a PipeWire remote that exposes the cameras, or
/// `None` when the portal reports no camera.
async fn portal() -> Result<Option<OwnedFd>, Error> {
	let portal = ashpd::desktop::camera::Camera::new()
		.await
		.map_err(|e| err("camera portal", e))?;
	if !portal.is_present().await.map_err(|e| err("camera portal", e))? {
		return Ok(None);
	}
	// The portal asks the user unless the sandbox's permission store already
	// holds a grant, so this blocks on the user the first time.
	portal
		.request_access(Default::default())
		.await
		.map_err(|e| err("camera portal access", e))?
		.response()
		.map_err(|e| Error::PermissionDenied(format!("camera access request: {e}")))?;
	let fd = portal
		.open_pipe_wire_remote(Default::default())
		.await
		.map_err(|e| err("camera portal pipewire remote", e))?;
	Ok(Some(fd))
}

/// Pick the camera and its mode for a stream on `core`: the node id to link
/// to, and the mode to offer.
pub(super) fn choose(
	mainloop: &pw::main_loop::MainLoopRc,
	core: &pw::core::CoreRc,
	name: Option<&str>,
	want: Request,
) -> Result<(u32, Selected), Error> {
	let registry = core.get_registry_rc().map_err(|e| err("pipewire registry", e))?;
	let nodes = scan(mainloop, core, &registry)?;
	let node = resolve(&nodes, name)?;
	let formats = formats(mainloop, core, &registry, node)?;
	let selected = select(&formats, want).ok_or_else(|| {
		Error::Codec(anyhow::anyhow!(
			"PipeWire camera {} offers no YUY2, NV12, RGB, or MJPEG mode",
			node.name
		))
	})?;
	Ok((node.id, selected))
}

/// A PipeWire camera node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Node {
	/// The registry id to link to. Only valid for this connection's lifetime.
	id: u32,
	/// `node.name`, which stays the same across restarts and reboots.
	name: String,
	/// `node.description`, or the best name the node has short of it.
	description: String,
	/// `priority.session`, which the session manager ranks default nodes by.
	priority: i32,
}

impl Node {
	/// Build a node from a registry global's properties, or `None` when it is
	/// not a camera. The criterion is the one xdg-desktop-portal applies when it
	/// decides which nodes a sandboxed app may see.
	fn from_props<'a>(id: u32, prop: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
		if prop(*pw::keys::MEDIA_CLASS)? != "Video/Source" || prop(*pw::keys::MEDIA_ROLE)? != "Camera" {
			return None;
		}
		let name = prop(*pw::keys::NODE_NAME).filter(|name| !name.is_empty())?;
		let description = prop(*pw::keys::NODE_DESCRIPTION)
			.or_else(|| prop(*pw::keys::NODE_NICK))
			.filter(|description| !description.is_empty())
			.unwrap_or(name);
		let priority = prop(*pw::keys::PRIORITY_SESSION)
			.and_then(|priority| priority.parse().ok())
			.unwrap_or(0);
		Some(Self {
			id,
			name: name.to_string(),
			description: description.to_string(),
			priority,
		})
	}
}

/// Run `mainloop` until the daemon has answered every request sent before
/// this one on `core`.
fn roundtrip(mainloop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) -> Result<(), Error> {
	let outcome: Rc<RefCell<Option<Result<(), Error>>>> = Rc::new(RefCell::new(None));
	let pending = core.sync(0).map_err(|e| err("pipewire sync", e))?;
	let _core = core
		.add_listener_local()
		.done({
			let outcome = outcome.clone();
			let mainloop = mainloop.downgrade();
			move |id, seq| {
				if id == pw::core::PW_ID_CORE && seq == pending {
					outcome.borrow_mut().get_or_insert(Ok(()));
					if let Some(mainloop) = mainloop.upgrade() {
						mainloop.quit();
					}
				}
			}
		})
		.error({
			let outcome = outcome.clone();
			let mainloop = mainloop.downgrade();
			move |id, _, _, message| {
				if id == pw::core::PW_ID_CORE {
					outcome.borrow_mut().get_or_insert(Err(err("pipewire", message)));
					if let Some(mainloop) = mainloop.upgrade() {
						mainloop.quit();
					}
				}
			}
		})
		.register();
	let timer = mainloop.loop_().add_timer({
		let mainloop = mainloop.downgrade();
		move |_| {
			if let Some(mainloop) = mainloop.upgrade() {
				mainloop.quit();
			}
		}
	});
	timer
		.update_timer(Some(QUERY_TIMEOUT), None)
		.into_result()
		.map_err(|e| err("pipewire timer", e))?;

	mainloop.run();
	let outcome = outcome.borrow_mut().take();
	outcome.unwrap_or_else(|| {
		Err(Error::Codec(anyhow::anyhow!(
			"PipeWire did not answer within {QUERY_TIMEOUT:?}"
		)))
	})
}

/// List the camera nodes in `registry`.
fn scan(
	mainloop: &pw::main_loop::MainLoopRc,
	core: &pw::core::CoreRc,
	registry: &pw::registry::RegistryRc,
) -> Result<Vec<Node>, Error> {
	let nodes = Rc::new(RefCell::new(Vec::new()));
	let _registry = registry
		.add_listener_local()
		.global({
			let nodes = nodes.clone();
			move |global| {
				if global.type_ != pw::types::ObjectType::Node {
					return;
				}
				let Some(props) = global.props else { return };
				if let Some(node) = Node::from_props(global.id, |key| props.get(key)) {
					nodes.borrow_mut().push(node);
				}
			}
		})
		.register();
	// The registry announces every existing global before the sync reply.
	roundtrip(mainloop, core)?;
	Ok(nodes.take())
}

/// The camera named `name`, or the one with the highest session priority,
/// which is the camera the session manager links by default. Fails when the
/// camera is not there, rather than leaving the stream unlinked until the
/// format wait runs out.
fn resolve<'a>(nodes: &'a [Node], name: Option<&str>) -> Result<&'a Node, Error> {
	let Some(name) = name else {
		// `max_by_key` keeps the last of equals; the registry's first is wanted.
		return nodes
			.iter()
			.rev()
			.max_by_key(|node| node.priority)
			.ok_or_else(|| Error::SourceUnavailable("PipeWire has no camera".to_string()));
	};
	nodes.iter().find(|node| node.name == name).ok_or_else(|| {
		let found: Vec<_> = nodes.iter().map(|node| node.name.as_str()).collect();
		Error::SourceUnavailable(format!("no PipeWire camera named {name} (found: {})", found.join(", ")))
	})
}

/// The raw formats `convert` turns into I420.
const RAW_FORMATS: [VideoFormat; 6] = [
	VideoFormat::YUY2,
	VideoFormat::NV12,
	VideoFormat::BGRx,
	VideoFormat::BGRA,
	VideoFormat::RGBx,
	VideoFormat::RGBA,
];

/// How a camera format carries its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
	/// A raw format [`super::convert`] handles.
	Raw(VideoFormat),
	/// Motion-JPEG, decoded per frame.
	Mjpeg,
}

impl Encoding {
	/// What converting to I420 costs, in the V4L2 backend's terms: resampling
	/// YUV is cheapest, then a color conversion, then a JPEG decode.
	fn cost(self) -> u8 {
		match self {
			Self::Raw(VideoFormat::YUY2 | VideoFormat::NV12) => 0,
			Self::Raw(_) => 1,
			Self::Mjpeg => 2,
		}
	}
}

/// One `EnumFormat` entry of a camera, for a format this backend converts.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Format {
	encoding: Encoding,
	/// The sizes on offer. A size range contributes its corners.
	sizes: Vec<Size>,
	/// The exact rates on offer, highest first. Empty for a rate range.
	rates: Vec<Rate>,
	/// The size range, when the size is one, which also offers the grid size
	/// nearest the request.
	ranged: Option<Grid>,
	/// The lowest and highest rate, when the rate is a continuous range.
	rate_range: Option<(spa::utils::Fraction, spa::utils::Fraction)>,
}

/// A size range: every size from `min` to `max` in `step` increments.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Grid {
	min: Size,
	max: Size,
	step: Size,
}

impl Grid {
	/// The size on the grid nearest `want` with even dimensions, the way a V4L2
	/// driver rounds a request for a stepwise size.
	fn nearest(&self, want: Size) -> Option<Size> {
		let snap = |value: u32, min: u32, max: u32, step: u32| {
			let (first, last) = bounds(min, max, step)?;
			let value = value.clamp(first, last);
			if first == last {
				return Some(first);
			}
			// `bounds` spans more than one value only on a nonzero step.
			let below = min + (value - min) / step * step;
			[below.checked_sub(step), Some(below), below.checked_add(step)]
				.into_iter()
				.flatten()
				.filter(|size| size.is_multiple_of(2) && (first..=last).contains(size))
				.min_by_key(|size| size.abs_diff(value))
		};
		Some(Size::new(
			snap(want.width, self.min.width, self.max.width, self.step.width)?,
			snap(want.height, self.min.height, self.max.height, self.step.height)?,
		))
	}

	/// The largest and smallest sizes on the grid with even dimensions, the way
	/// the V4L2 backend reports a stepwise size.
	fn corners(&self) -> Vec<Size> {
		let width = bounds(self.min.width, self.max.width, self.step.width);
		let height = bounds(self.min.height, self.max.height, self.step.height);
		let (Some((min_width, max_width)), Some((min_height, max_height))) = (width, height) else {
			return Vec::new();
		};
		let (largest, smallest) = (Size::new(max_width, max_height), Size::new(min_width, min_height));
		if largest == smallest {
			vec![largest]
		} else {
			vec![largest, smallest]
		}
	}
}

/// Whether the rate range `min..=max` holds `rate`, compared as cross products
/// because a range may start at 0/1, which is no valid [`Rate`].
fn holds(min: spa::utils::Fraction, max: spa::utils::Fraction, rate: Rate) -> bool {
	let (num, denom) = (u64::from(rate.numerator()), u64::from(rate.denominator()));
	u64::from(min.num) * denom <= num * u64::from(min.denom) && num * u64::from(max.denom) <= u64::from(max.num) * denom
}

/// The mode a camera stream offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Selected {
	encoding: Encoding,
	size: Size,
	/// `None` when the camera reports a rate range and no rate in it was requested.
	framerate: Option<Rate>,
}

impl Selected {
	/// Serialize the `EnumFormat` offers for exactly this mode, a DMA-BUF offer
	/// first when the format has a DRM fourcc to import as.
	pub(super) fn offers(&self) -> Vec<Vec<u8>> {
		let dmabuf = match self.encoding {
			Encoding::Raw(format) => drm_format(format).is_some(),
			Encoding::Mjpeg => false,
		};
		let mut offers = Vec::new();
		if dmabuf {
			offers.push(self.offer(true));
		}
		offers.push(self.offer(false));
		offers
	}

	fn offer(&self, dmabuf: bool) -> Vec<u8> {
		let property = |key: FormatProperties, value| spa::pod::Property::new(key.as_raw(), value);
		let mut properties = vec![property(
			FormatProperties::MediaType,
			Value::Id(spa::utils::Id(MediaType::Video.as_raw())),
		)];
		match self.encoding {
			Encoding::Raw(format) => {
				properties.push(property(
					FormatProperties::MediaSubtype,
					Value::Id(spa::utils::Id(MediaSubtype::Raw.as_raw())),
				));
				properties.push(property(
					FormatProperties::VideoFormat,
					Value::Id(spa::utils::Id(format.as_raw())),
				));
			}
			Encoding::Mjpeg => properties.push(property(
				FormatProperties::MediaSubtype,
				Value::Id(spa::utils::Id(MediaSubtype::Mjpg.as_raw())),
			)),
		}
		properties.push(property(
			FormatProperties::VideoSize,
			Value::Rectangle(spa::utils::Rectangle {
				width: self.size.width,
				height: self.size.height,
			}),
		));
		let framerate = match self.framerate {
			Some(rate) => Value::Fraction(fraction(rate)),
			None => Value::Choice(ChoiceValue::Fraction(spa::utils::Choice(
				spa::utils::ChoiceFlags::empty(),
				ChoiceEnum::Range {
					default: spa::utils::Fraction {
						num: DEFAULT_FRAMERATE,
						denom: 1,
					},
					min: spa::utils::Fraction { num: 0, denom: 1 },
					max: spa::utils::Fraction { num: 1000, denom: 1 },
				},
			))),
		};
		properties.push(property(FormatProperties::VideoFramerate, framerate));
		if dmabuf {
			properties.push(linear_modifier());
		}
		serialize_format(spa::pod::Object {
			type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
			id: spa::param::ParamType::EnumFormat.as_raw(),
			properties,
		})
	}
}

fn fraction(rate: Rate) -> spa::utils::Fraction {
	spa::utils::Fraction {
		num: rate.numerator(),
		denom: rate.denominator(),
	}
}

/// List the formats a camera node offers that this backend converts.
fn formats(
	mainloop: &pw::main_loop::MainLoopRc,
	core: &pw::core::CoreRc,
	registry: &pw::registry::RegistryRc,
	node: &Node,
) -> Result<Vec<Format>, Error> {
	let proxy: pw::node::Node = registry
		.bind(&pw::registry::GlobalObject {
			id: node.id,
			permissions: pw::permissions::PermissionFlags::empty(),
			type_: pw::types::ObjectType::Node,
			version: 0,
			props: None::<&spa::utils::dict::DictRef>,
		})
		.map_err(|e| err("pipewire node", e))?;
	let formats = Rc::new(RefCell::new(Vec::new()));
	let _listener = proxy
		.add_listener_local()
		.param({
			let formats = formats.clone();
			move |_, id, _, _, param| {
				if id != spa::param::ParamType::EnumFormat {
					return;
				}
				let Some(param) = param else { return };
				match spa::pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes()) {
					Ok((_, value)) => formats.borrow_mut().extend(parse_format(&value)),
					Err(e) => tracing::debug!(error = ?e, "unreadable PipeWire EnumFormat"),
				}
			}
		})
		.register();
	proxy.enum_params(0, Some(spa::param::ParamType::EnumFormat), 0, u32::MAX);
	roundtrip(mainloop, core)?;
	Ok(formats.take())
}

/// Read one `EnumFormat` or `Format` pod into the formats it offers that this
/// backend converts. A pod may list several pixel formats in one choice.
fn parse_format(value: &Value) -> Vec<Format> {
	let Value::Object(object) = value else {
		return Vec::new();
	};
	let property = |key: FormatProperties| {
		object
			.properties
			.iter()
			.find(|property| property.key == key.as_raw())
			.map(|property| &property.value)
	};
	if property(FormatProperties::MediaType) != Some(&Value::Id(spa::utils::Id(MediaType::Video.as_raw()))) {
		return Vec::new();
	}
	let encodings = match property(FormatProperties::MediaSubtype) {
		Some(Value::Id(id)) if *id == spa::utils::Id(MediaSubtype::Mjpg.as_raw()) => vec![Encoding::Mjpeg],
		Some(Value::Id(id)) if *id == spa::utils::Id(MediaSubtype::Raw.as_raw()) => {
			let formats = match property(FormatProperties::VideoFormat) {
				Some(Value::Id(id)) => vec![*id],
				Some(Value::Choice(ChoiceValue::Id(choice))) => listed(&choice.1),
				_ => Vec::new(),
			};
			formats
				.into_iter()
				.map(|id| VideoFormat::from_raw(id.0))
				.filter(|format| RAW_FORMATS.contains(format))
				.map(Encoding::Raw)
				.collect()
		}
		_ => Vec::new(),
	};

	let size = |rectangle: &spa::utils::Rectangle| Size::new(rectangle.width, rectangle.height);
	let (sizes, ranged) = match property(FormatProperties::VideoSize) {
		Some(Value::Rectangle(rectangle)) => (vec![size(rectangle)], None),
		Some(Value::Choice(ChoiceValue::Rectangle(choice))) => {
			let grid = match &choice.1 {
				ChoiceEnum::Range { min, max, .. } => Some(Grid {
					min: size(min),
					max: size(max),
					step: Size::new(1, 1),
				}),
				ChoiceEnum::Step { min, max, step, .. } => Some(Grid {
					min: size(min),
					max: size(max),
					step: size(step),
				}),
				_ => None,
			};
			match grid {
				Some(grid) => (grid.corners(), Some(grid)),
				None => (listed(&choice.1).iter().map(size).collect(), None),
			}
		}
		_ => (Vec::new(), None),
	};
	let rate = |fraction: &spa::utils::Fraction| Rate::new(fraction.num, fraction.denom).ok();
	let framerate = property(FormatProperties::VideoFramerate);
	let mut rates: Vec<Rate> = match framerate {
		Some(Value::Fraction(fraction)) => rate(fraction).into_iter().collect(),
		Some(Value::Choice(ChoiceValue::Fraction(choice))) => listed(&choice.1).iter().filter_map(rate).collect(),
		_ => Vec::new(),
	};
	rates.sort_by(|left, right| right.cmp(left));
	rates.dedup();
	// Only a continuous range: whether a rate lies on a stepwise range's grid
	// is the driver's call, so that offer stays open.
	let rate_range = match framerate {
		Some(Value::Choice(ChoiceValue::Fraction(spa::utils::Choice(_, ChoiceEnum::Range { min, max, .. })))) => {
			Some((*min, *max))
		}
		_ => None,
	};

	encodings
		.into_iter()
		.map(|encoding| Format {
			encoding,
			sizes: sizes.clone(),
			rates: rates.clone(),
			ranged,
			rate_range,
		})
		.collect()
}

/// The values a choice lists. A range lists none: it describes a continuum.
fn listed<T: Copy + PartialEq + spa::pod::CanonicalFixedSizedPod>(choice: &ChoiceEnum<T>) -> Vec<T> {
	let values = match choice {
		ChoiceEnum::None(value) => vec![*value],
		// SPA usually repeats the default among the alternatives.
		ChoiceEnum::Enum { default, alternatives } => {
			std::iter::once(*default).chain(alternatives.iter().copied()).collect()
		}
		ChoiceEnum::Range { .. } | ChoiceEnum::Step { .. } | ChoiceEnum::Flags { .. } => Vec::new(),
	};
	let mut unique = Vec::with_capacity(values.len());
	for value in values {
		if !unique.contains(&value) {
			unique.push(value);
		}
	}
	unique
}

/// Pick the mode nearest `want` with the V4L2 backend's rules. A size range
/// offers its size nearest the request, and a rate range that holds the
/// requested rate offers that rate.
fn select(formats: &[Format], want: Request) -> Option<Selected> {
	let mut candidates = Vec::new();
	for format in formats {
		let sizes = format
			.ranged
			.and_then(|grid| grid.nearest(want.size))
			.into_iter()
			.chain(format.sizes.iter().copied());
		let rates: Vec<Option<Rate>> = if format.rates.is_empty() {
			let held = want
				.framerate
				.filter(|&rate| format.rate_range.is_some_and(|(min, max)| holds(min, max, rate)));
			vec![held]
		} else {
			format.rates.iter().copied().map(Some).collect()
		};
		for size in sizes {
			for &framerate in &rates {
				candidates.push(Selected {
					encoding: format.encoding,
					size,
					framerate,
				});
			}
		}
	}
	mode::nearest(candidates, want, |selected| mode::Candidate {
		size: selected.size,
		framerate: selected.framerate,
		cost: selected.encoding.cost(),
	})
}

/// The negotiated MJPEG `Format` as the loop's `VideoInfoRaw`, with the
/// `Encoded` format standing for MJPEG and the negotiated size and rate.
pub(super) fn mjpeg_format(param: &spa::pod::Pod) -> Option<VideoInfoRaw> {
	let (_, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes()).ok()?;
	let format = parse_format(&value).into_iter().next()?;
	if format.encoding != Encoding::Mjpeg {
		return None;
	}
	let size = *format.sizes.first()?;
	let mut info = VideoInfoRaw::default();
	info.set_format(VideoFormat::Encoded);
	info.set_size(spa::utils::Rectangle {
		width: size.width,
		height: size.height,
	});
	if let Some(rate) = format.rates.first() {
		info.set_framerate(fraction(*rate));
	}
	Some(info)
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use super::*;

	fn node(props: &[(&str, &str)]) -> Option<Node> {
		let props: HashMap<_, _> = props.iter().copied().collect();
		Node::from_props(62, |key| props.get(key).copied())
	}

	const WEBCAM: [(&str, &str); 5] = [
		("media.class", "Video/Source"),
		("media.role", "Camera"),
		("node.name", "v4l2_input.pci-0000_00_14.0-usb-0_4_1.0"),
		("node.description", "Integrated Camera (V4L2)"),
		("priority.session", "1000"),
	];

	#[test]
	fn camera_nodes_are_video_sources_with_the_camera_role() {
		assert_eq!(
			node(&WEBCAM),
			Some(Node {
				id: 62,
				name: "v4l2_input.pci-0000_00_14.0-usb-0_4_1.0".to_string(),
				description: "Integrated Camera (V4L2)".to_string(),
				priority: 1000,
			})
		);

		let mut screen = WEBCAM;
		screen[1] = ("media.role", "Screen");
		assert_eq!(node(&screen), None);
		let mut sink = WEBCAM;
		sink[0] = ("media.class", "Video/Sink");
		assert_eq!(node(&sink), None);
		// The name is the id's only stable part, so a node without one is unusable.
		assert_eq!(node(&WEBCAM[..2]), None);
	}

	#[test]
	fn a_camera_without_a_description_falls_back_to_its_nick_then_name() {
		let libcamera = [
			("media.class", "Video/Source"),
			("media.role", "Camera"),
			("node.name", "libcamera_input./base/soc/i2c0mux/i2c@1/imx708@1a"),
			("node.nick", "imx708"),
		];
		assert_eq!(node(&libcamera).unwrap().description, "imx708");
		assert_eq!(
			node(&libcamera[..3]).unwrap().description,
			"libcamera_input./base/soc/i2c0mux/i2c@1/imx708@1a"
		);
	}

	#[test]
	fn resolve_finds_the_named_camera_or_the_highest_priority_one() {
		let webcam = node(&WEBCAM).unwrap();
		let infrared = Node {
			id: 79,
			name: "v4l2_input.pci-0000_00_14.0-usb-0_4_1.2".to_string(),
			priority: 980,
			..webcam.clone()
		};
		let nodes = [infrared.clone(), webcam.clone()];
		assert_eq!(resolve(&nodes, Some(&infrared.name)).unwrap().id, 79);
		assert_eq!(resolve(&nodes, None).unwrap().id, 62);
		// Equal priorities keep the registry's order.
		let tied = [
			Node {
				priority: 1000,
				..infrared
			},
			webcam,
		];
		assert_eq!(resolve(&tied, None).unwrap().id, 79);

		let error = resolve(&nodes, Some("missing")).unwrap_err().to_string();
		assert!(error.contains("v4l2_input.pci-0000_00_14.0-usb-0_4_1.0"), "{error}");
		assert!(matches!(resolve(&[], None), Err(Error::SourceUnavailable(_))));
	}

	fn rectangle(width: u32, height: u32) -> spa::utils::Rectangle {
		spa::utils::Rectangle { width, height }
	}

	/// One `EnumFormat` entry the way spa-v4l2 reports it for a UVC webcam.
	fn enum_format(subtype: MediaSubtype, format: Option<VideoFormat>, size: spa::utils::Rectangle) -> Value {
		let mut properties = vec![
			spa::pod::Property::new(
				FormatProperties::MediaType.as_raw(),
				Value::Id(spa::utils::Id(MediaType::Video.as_raw())),
			),
			spa::pod::Property::new(
				FormatProperties::MediaSubtype.as_raw(),
				Value::Id(spa::utils::Id(subtype.as_raw())),
			),
			spa::pod::Property::new(FormatProperties::VideoSize.as_raw(), Value::Rectangle(size)),
			spa::pod::Property::new(
				FormatProperties::VideoFramerate.as_raw(),
				Value::Choice(ChoiceValue::Fraction(spa::utils::Choice(
					spa::utils::ChoiceFlags::empty(),
					ChoiceEnum::None(spa::utils::Fraction { num: 30, denom: 1 }),
				))),
			),
		];
		if let Some(format) = format {
			properties.push(spa::pod::Property::new(
				FormatProperties::VideoFormat.as_raw(),
				Value::Id(spa::utils::Id(format.as_raw())),
			));
		}
		Value::Object(spa::pod::Object {
			type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
			id: spa::param::ParamType::EnumFormat.as_raw(),
			properties,
		})
	}

	/// The integrated webcam's list: MJPEG up to 1080p, YUY2 only at VGA sizes,
	/// which is why the first mode is no answer to a size request.
	fn webcam_formats() -> Vec<Format> {
		[
			enum_format(MediaSubtype::Mjpg, None, rectangle(1280, 720)),
			enum_format(MediaSubtype::Mjpg, None, rectangle(640, 480)),
			enum_format(MediaSubtype::Mjpg, None, rectangle(1920, 1080)),
			enum_format(MediaSubtype::Raw, Some(VideoFormat::YUY2), rectangle(640, 480)),
			enum_format(MediaSubtype::Raw, Some(VideoFormat::YUY2), rectangle(640, 360)),
			enum_format(MediaSubtype::Raw, Some(VideoFormat::GRAY8), rectangle(640, 360)),
		]
		.iter()
		.flat_map(parse_format)
		.collect()
	}

	#[test]
	fn enum_formats_parse_to_the_convertible_formats() {
		let formats = webcam_formats();
		// GRAY8 is not converted, so it is not a candidate.
		assert_eq!(formats.len(), 5);
		assert_eq!(formats[0].encoding, Encoding::Mjpeg);
		assert_eq!(formats[0].sizes, [Size::new(1280, 720)]);
		assert_eq!(formats[0].rates, [Rate::new(30, 1).unwrap()]);
		assert_eq!(formats[3].encoding, Encoding::Raw(VideoFormat::YUY2));
	}

	#[test]
	fn selection_matches_the_v4l2_rules() {
		let formats = webcam_formats();
		let select = |width, height| {
			let selected = select(
				&formats,
				Request {
					size: Size::new(width, height),
					framerate: None,
				},
			)
			.unwrap();
			(selected.encoding, selected.size)
		};
		// An exact size wins, and the cheaper YUY2 wins a tie with MJPEG.
		assert_eq!(
			select(640, 480),
			(Encoding::Raw(VideoFormat::YUY2), Size::new(640, 480))
		);
		assert_eq!(
			select(640, 360),
			(Encoding::Raw(VideoFormat::YUY2), Size::new(640, 360))
		);
		assert_eq!(select(1920, 1080), (Encoding::Mjpeg, Size::new(1920, 1080)));
		// No exact size: the nearest one, whatever format carries it.
		assert_eq!(select(1280, 800), (Encoding::Mjpeg, Size::new(1280, 720)));
	}

	/// An NV12 `EnumFormat` entry with the given size and rate properties, the way
	/// spa-libcamera reports a sensor.
	fn nv12_format(size: Value, framerate: Option<Value>) -> Vec<Format> {
		let property = |key: FormatProperties, value| spa::pod::Property::new(key.as_raw(), value);
		let mut properties = vec![
			property(
				FormatProperties::MediaType,
				Value::Id(spa::utils::Id(MediaType::Video.as_raw())),
			),
			property(
				FormatProperties::MediaSubtype,
				Value::Id(spa::utils::Id(MediaSubtype::Raw.as_raw())),
			),
			property(
				FormatProperties::VideoFormat,
				Value::Id(spa::utils::Id(VideoFormat::NV12.as_raw())),
			),
			property(FormatProperties::VideoSize, size),
		];
		properties.extend(framerate.map(|framerate| property(FormatProperties::VideoFramerate, framerate)));
		parse_format(&Value::Object(spa::pod::Object {
			type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
			id: spa::param::ParamType::EnumFormat.as_raw(),
			properties,
		}))
	}

	fn size_choice(choice: ChoiceEnum<spa::utils::Rectangle>) -> Value {
		Value::Choice(ChoiceValue::Rectangle(spa::utils::Choice(
			spa::utils::ChoiceFlags::empty(),
			choice,
		)))
	}

	fn ratio(num: u32, denom: u32) -> spa::utils::Fraction {
		spa::utils::Fraction { num, denom }
	}

	fn request(width: u32, height: u32, fps: Option<u32>) -> Request {
		Request {
			size: Size::new(width, height),
			framerate: fps.map(|fps| Rate::new(fps, 1).unwrap()),
		}
	}

	#[test]
	fn a_size_range_offers_the_requested_size() {
		let formats = nv12_format(
			size_choice(ChoiceEnum::Range {
				default: rectangle(1920, 1080),
				min: rectangle(64, 64),
				max: rectangle(4608, 2592),
			}),
			None,
		);
		assert_eq!(formats[0].sizes, [Size::new(4608, 2592), Size::new(64, 64)]);
		let selected = select(&formats, request(1280, 720, None)).unwrap();
		assert_eq!((selected.size, selected.framerate), (Size::new(1280, 720), None));
		// An odd request lands on the nearest even size rather than a corner.
		let selected = select(&formats, request(1281, 721, None)).unwrap();
		assert_eq!(selected.size, Size::new(1280, 720));
	}

	/// A stepwise range offers only sizes on its grid, and its corners move to
	/// even sizes the way `v4l2::bounds` moves a stepwise V4L2 size.
	#[test]
	fn a_stepwise_size_range_offers_the_nearest_even_size_on_its_grid() {
		let eights = nv12_format(
			size_choice(ChoiceEnum::Step {
				default: rectangle(640, 480),
				min: rectangle(32, 32),
				max: rectangle(1921, 1081),
				step: rectangle(8, 8),
			}),
			None,
		);
		assert_eq!(eights[0].sizes, [Size::new(1920, 1080), Size::new(32, 32)]);
		assert_eq!(
			select(&eights, request(1280, 720, None)).unwrap().size,
			Size::new(1280, 720)
		);
		assert_eq!(
			select(&eights, request(1270, 716, None)).unwrap().size,
			Size::new(1272, 712)
		);
		assert_eq!(
			select(&eights, request(4000, 3000, None)).unwrap().size,
			Size::new(1920, 1080)
		);

		// An odd step alternates parity, so the nearest grid size may be odd.
		let threes = nv12_format(
			size_choice(ChoiceEnum::Step {
				default: rectangle(640, 480),
				min: rectangle(1, 1),
				max: rectangle(1919, 1079),
				step: rectangle(3, 3),
			}),
			None,
		);
		assert_eq!(threes[0].sizes, [Size::new(1918, 1078), Size::new(4, 4)]);
		assert_eq!(
			select(&threes, request(1280, 720, None)).unwrap().size,
			Size::new(1282, 718)
		);

		// An even step from an odd minimum has no even size at all.
		let odd = nv12_format(
			size_choice(ChoiceEnum::Step {
				default: rectangle(641, 481),
				min: rectangle(1, 1),
				max: rectangle(1919, 1079),
				step: rectangle(2, 2),
			}),
			None,
		);
		assert!(odd[0].sizes.is_empty());
		assert_eq!(select(&odd, request(1280, 720, None)), None);
	}

	/// A rate range that holds the requested rate ranks as that rate, so an exact
	/// rate elsewhere does not beat it.
	#[test]
	fn a_rate_range_offers_the_requested_rate() {
		let range = Value::Choice(ChoiceValue::Fraction(spa::utils::Choice(
			spa::utils::ChoiceFlags::empty(),
			ChoiceEnum::Range {
				default: ratio(30, 1),
				min: ratio(0, 1),
				max: ratio(60, 1),
			},
		)));
		let mut formats = nv12_format(Value::Rectangle(rectangle(1280, 720)), Some(range));
		// A rate range lists no exact rate.
		assert!(formats[0].rates.is_empty());
		formats.extend(parse_format(&enum_format(
			MediaSubtype::Mjpg,
			None,
			rectangle(1280, 720),
		)));

		let pick = |fps| {
			let selected = select(&formats, request(1280, 720, fps)).unwrap();
			(selected.encoding, selected.framerate)
		};
		let nv12 = Encoding::Raw(VideoFormat::NV12);
		let rate = |fps| Some(Rate::new(fps, 1).unwrap());
		assert_eq!(pick(Some(60)), (nv12, rate(60)));
		assert_eq!(pick(Some(30)), (nv12, rate(30)));
		// Out of the range, the exact rate elsewhere is the better answer.
		assert_eq!(pick(Some(120)), (Encoding::Mjpeg, rate(30)));
		// Nothing requested: the range stays open and the cheaper format wins.
		assert_eq!(pick(None), (nv12, None));

		assert!(holds(ratio(0, 1), ratio(30, 1), Rate::new(30000, 1001).unwrap()));
		assert!(!holds(ratio(0, 1), ratio(30, 1), Rate::new(30001, 1000).unwrap()));
	}

	/// The offer pins the chosen mode, and only NV12 gets a DMA-BUF offer.
	#[test]
	fn offers_pin_the_selected_mode() {
		let decode = |bytes: &Vec<u8>| {
			let (_, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(bytes).unwrap();
			let Value::Object(object) = value else {
				panic!("offer is not an object");
			};
			object
		};
		let find = |object: &spa::pod::Object, key: FormatProperties| {
			object
				.properties
				.iter()
				.find(|property| property.key == key.as_raw())
				.map(|property| property.value.clone())
		};

		let mjpeg = Selected {
			encoding: Encoding::Mjpeg,
			size: Size::new(1920, 1080),
			framerate: Some(Rate::new(30, 1).unwrap()),
		};
		let offers = mjpeg.offers();
		assert_eq!(offers.len(), 1);
		let offer = decode(&offers[0]);
		assert_eq!(
			find(&offer, FormatProperties::MediaSubtype),
			Some(Value::Id(spa::utils::Id(MediaSubtype::Mjpg.as_raw())))
		);
		assert_eq!(
			find(&offer, FormatProperties::VideoSize),
			Some(Value::Rectangle(rectangle(1920, 1080)))
		);
		assert_eq!(
			find(&offer, FormatProperties::VideoFramerate),
			Some(Value::Fraction(spa::utils::Fraction { num: 30, denom: 1 }))
		);
		// The negotiated format parses back into the loop's MJPEG description.
		let info = mjpeg_format(spa::pod::Pod::from_bytes(&offers[0]).unwrap()).unwrap();
		assert_eq!(info.format(), VideoFormat::Encoded);
		assert_eq!(info.size(), rectangle(1920, 1080));

		let nv12 = Selected {
			encoding: Encoding::Raw(VideoFormat::NV12),
			..mjpeg
		};
		let offers = nv12.offers();
		assert_eq!(offers.len(), 2);
		assert!(find(&decode(&offers[0]), FormatProperties::VideoModifier).is_some());
		assert!(find(&decode(&offers[1]), FormatProperties::VideoModifier).is_none());
		let yuy2 = Selected {
			encoding: Encoding::Raw(VideoFormat::YUY2),
			..mjpeg
		};
		assert_eq!(yuy2.offers().len(), 1);
	}

	/// Lists the PipeWire cameras over the session socket. Needs no camera and
	/// never turns one on, so a host without PipeWire just lists nothing.
	#[tokio::test]
	async fn lists_cameras_over_the_session_socket() {
		if ashpd::is_sandboxed() {
			eprintln!("skipping: listing inside a sandbox would ask the camera portal");
			return;
		}
		let cameras = cameras().await.expect("listing PipeWire cameras");
		for camera in &cameras {
			assert!(camera.id.starts_with("pipewire:"), "{}", camera.id);
			assert!(!camera.name.is_empty());
		}
		eprintln!("PipeWire cameras: {cameras:?}");
	}

	/// Open `config` and check that five frames arrive at the reported size.
	async fn capture(config: &Config) -> Result<Stream, Error> {
		let mut stream = crate::capture::open(config).await?;
		assert!(stream.width() >= 2 && stream.width().is_multiple_of(2), "bad width");
		assert!(stream.height() >= 2 && stream.height().is_multiple_of(2), "bad height");
		for i in 0..5 {
			let frame = stream.read().await?.unwrap_or_else(|| panic!("no frame {i}"));
			assert_eq!(frame.surface.width(), stream.width());
			assert_eq!(frame.surface.height(), stream.height());
		}
		eprintln!(
			"{}: captured 5 frames at {}x{}, {:?} fps, color {:?}",
			stream.label(),
			stream.width(),
			stream.height(),
			stream.framerate(),
			stream.color()
		);
		Ok(stream)
	}

	/// Captures from each PipeWire camera over the session socket, at its
	/// smallest mode and at its largest mode up to 1080p, and checks the stream
	/// lands on the requested size. Ignored because it turns the cameras on:
	/// `cargo test -p moq-video --features pipewire pipewire_camera -- --ignored`.
	#[tokio::test]
	#[ignore = "turns on every PipeWire camera on the host"]
	async fn pipewire_camera_captures_frames() {
		if ashpd::is_sandboxed() {
			eprintln!("skipping: capturing inside a sandbox would ask the camera portal");
			return;
		}
		let cameras = cameras().await.expect("listing PipeWire cameras");
		if cameras.is_empty() {
			eprintln!("skipping: no PipeWire camera");
			return;
		}

		let mut captured = 0;
		for camera in cameras {
			let modes = crate::capture::camera_modes(Some(&camera.id))
				.await
				.expect("listing PipeWire camera modes");
			eprintln!("{}: modes {modes:?}", camera.id);
			// An IR camera offers only GRAY8, which this backend does not convert.
			let Some(smallest) = modes.last() else {
				eprintln!("{}: no convertible mode", camera.id);
				continue;
			};
			let largest = modes
				.iter()
				.find(|mode| mode.width <= 1920 && mode.height <= 1080)
				.unwrap_or(smallest);
			for mode in [smallest, largest] {
				let config = Config {
					source: camera.source(),
					width: Some(mode.width),
					height: Some(mode.height),
					framerate: mode.max_framerate(),
					..Default::default()
				};
				let stream = capture(&config)
					.await
					.unwrap_or_else(|error| panic!("{} at {}x{}: {error}", camera.id, mode.width, mode.height));
				assert_eq!(stream.label(), camera.id);
				assert_eq!((stream.width(), stream.height()), (mode.width, mode.height));
				assert_eq!(stream.framerate(), mode.max_framerate());
			}
			captured += 1;
		}
		assert!(captured > 0, "no PipeWire camera could be captured");

		// `pipewire` alone picks the highest-priority camera, at the default size.
		let config = Config {
			source: crate::capture::Source::Camera(Some(PIPEWIRE.to_string())),
			..Default::default()
		};
		let stream = capture(&config).await.expect("default PipeWire camera");
		assert_eq!(stream.label(), PIPEWIRE);
	}
}
