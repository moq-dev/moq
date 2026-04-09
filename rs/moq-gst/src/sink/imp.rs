//! Async-friendly MoqSink that keeps the original dynamic-pad Element
//! behavior while pushing all network setup and CMAF publishing work into
//! a Tokio task. The GLib state change thread never blocks, pads still get
//! requested dynamically, and each pad simply forwards buffers/events to the
//! background worker via an unbounded channel.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use tokio::sync::mpsc;
use url::Url;

use hang::moq_lite;

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("spawn tokio runtime")
});

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
	gst::DebugCategory::new(
		"moq-sink",
		gst::DebugColorFlags::empty(),
		Some("MoQ Sink (async element)"),
	)
});

#[derive(Debug, Clone, Default)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
	tls_disable_verify: bool,
}

#[derive(Debug, Clone)]
struct ResolvedSettings {
	url: Url,
	broadcast: String,
	tls_disable_verify: bool,
}

impl TryFrom<Settings> for ResolvedSettings {
	type Error = anyhow::Error;

	fn try_from(value: Settings) -> Result<Self> {
		Ok(Self {
			url: Url::parse(value.url.as_ref().context("url property is required")?)?,
			broadcast: value
				.broadcast
				.as_ref()
				.context("broadcast property is required")?
				.clone(),
			tls_disable_verify: value.tls_disable_verify,
		})
	}
}

#[derive(Debug)]
struct SessionHandle {
	sender: mpsc::UnboundedSender<ControlMessage>,
	join: tokio::task::JoinHandle<()>,
}

impl SessionHandle {
	fn stop(self) {
		let _ = self.sender.send(ControlMessage::Shutdown);
		RUNTIME.spawn(async move {
			if let Err(err) = self.join.await {
				gst::warning!(CAT, "session task ended with error: {err:?}");
			}
		});
	}
}

struct PadState {
	decoder: moq_mux::import::Decoder,
	reference_pts: Option<gst::ClockTime>,
}

struct RuntimeState {
	#[allow(dead_code)]
	session: moq_lite::Session,
	broadcast: moq_lite::BroadcastProducer,
	catalog: moq_mux::CatalogProducer,
	pads: HashMap<String, PadState>,
}

#[derive(Debug)]
enum ControlMessage {
	SetCaps {
		pad_name: String,
		caps: gst::Caps,
	},
	Buffer {
		pad_name: String,
		data: Bytes,
		pts: Option<gst::ClockTime>,
	},
	DropPad {
		pad_name: String,
	},
	Shutdown,
}

#[derive(Default)]
pub struct MoqSink {
	settings: Mutex<Settings>,
	session: Mutex<Option<SessionHandle>>,
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSink {
	const NAME: &'static str = "MoqSink";
	type Type = super::MoqSink;
	type ParentType = gst::Element;
}

impl ObjectImpl for MoqSink {
	fn properties() -> &'static [glib::ParamSpec] {
		static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
			vec![
				glib::ParamSpecString::builder("url")
					.nick("Source URL")
					.blurb("Connect to the given URL")
					.build(),
				glib::ParamSpecString::builder("broadcast")
					.nick("Broadcast")
					.blurb("The name of the broadcast to publish")
					.build(),
				glib::ParamSpecBoolean::builder("tls-disable-verify")
					.nick("TLS disable verify")
					.blurb("Disable TLS verification")
					.default_value(false)
					.build(),
			]
		});
		PROPS.as_ref()
	}

	fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
		let mut settings = self.settings.lock().unwrap();
		match pspec.name() {
			"url" => settings.url = value.get().unwrap(),
			"broadcast" => settings.broadcast = value.get().unwrap(),
			"tls-disable-verify" => settings.tls_disable_verify = value.get().unwrap(),
			_ => unreachable!(),
		}
	}

	fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
		let settings = self.settings.lock().unwrap();
		match pspec.name() {
			"url" => settings.url.to_value(),
			"broadcast" => settings.broadcast.to_value(),
			"tls-disable-verify" => settings.tls_disable_verify.to_value(),
			_ => unreachable!(),
		}
	}
}

impl GstObjectImpl for MoqSink {}

impl ElementImpl for MoqSink {
	fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
		static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
			gst::subclass::ElementMetadata::new(
				"MoQ Sink (async)",
				"Sink/Network/MoQ",
				"Transmits media over MoQ",
				"Luke Curley <kixelated@gmail.com>, Steve McFarlin <steve@stevemcfarlin.com>",
			)
		});
		Some(&*ELEMENT_METADATA)
	}

	fn pad_templates() -> &'static [gst::PadTemplate] {
		static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
			let mut caps = gst::Caps::new_empty();
			caps.merge(
				gst::Caps::builder("video/x-h264")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build(),
			);
			caps.merge(
				gst::Caps::builder("video/x-h265")
					.field("stream-format", "byte-stream")
					.field("alignment", "au")
					.build(),
			);
			caps.merge(gst::Caps::builder("video/x-av1").build());
			caps.merge(
				gst::Caps::builder("audio/mpeg")
					.field("mpegversion", 4i32)
					.field("stream-format", "raw")
					.build(),
			);
			caps.merge(gst::Caps::builder("audio/x-opus").build());

			let templ =
				gst::PadTemplate::new("sink_%u", gst::PadDirection::Sink, gst::PadPresence::Request, &caps).unwrap();
			vec![templ]
		});
		PAD_TEMPLATES.as_ref()
	}

	fn request_new_pad(
		&self,
		templ: &gst::PadTemplate,
		name: Option<&str>,
		_caps: Option<&gst::Caps>,
	) -> Option<gst::Pad> {
		let pad_builder = gst::Pad::builder_from_template(templ)
			.chain_function(|pad, parent, buffer| {
				let element = parent
					.and_then(|p| p.downcast_ref::<super::MoqSink>())
					.ok_or(gst::FlowError::Error)?;
				element.imp().forward_buffer(pad, buffer)
			})
			.event_function(|pad, parent, event| {
				let Some(element) = parent.and_then(|p| p.downcast_ref::<super::MoqSink>()) else {
					return false;
				};
				element.imp().forward_event(pad, event)
			});

		let pad = if let Some(name) = name {
			pad_builder.name(name).build()
		} else {
			pad_builder.build()
		};

		self.obj().add_pad(&pad).ok()?;
		Some(pad)
	}

	fn release_pad(&self, pad: &gst::Pad) {
		if let Some(session) = self.session.lock().unwrap().as_ref() {
			let _ = session.sender.send(ControlMessage::DropPad {
				pad_name: pad.name().to_string(),
			});
		}
		let _ = self.obj().remove_pad(pad);
	}

	fn change_state(&self, transition: gst::StateChange) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
		match transition {
			gst::StateChange::ReadyToPaused => {
				self.start_session().map_err(|err| {
					gst::error!(CAT, obj = self.obj(), "failed to start session: {err:#}");
					gst::StateChangeError
				})?;
			}
			gst::StateChange::PausedToReady => self.stop_session(),
			_ => (),
		}

		self.parent_change_state(transition)
	}
}

impl MoqSink {
	fn start_session(&self) -> Result<()> {
		let settings = {
			let settings = self.settings.lock().unwrap().clone();
			ResolvedSettings::try_from(settings)?
		};

		let (tx, rx) = mpsc::unbounded_channel::<ControlMessage>();
		let join = RUNTIME.spawn(async move {
			if let Err(err) = run_session(settings, rx).await {
				gst::error!(CAT, "session error: {err:#}");
			}
		});

		*self.session.lock().unwrap() = Some(SessionHandle { sender: tx, join });
		Ok(())
	}

	fn stop_session(&self) {
		if let Some(handle) = self.session.lock().unwrap().take() {
			handle.stop();
		}
	}

	fn forward_buffer(&self, pad: &gst::Pad, buffer: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
		let sender = self
			.session
			.lock()
			.unwrap()
			.as_ref()
			.map(|handle| handle.sender.clone())
			.ok_or(gst::FlowError::Flushing)?;

		let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
		let pts = buffer.pts();
		let data = Bytes::copy_from_slice(map.as_slice());

		sender
			.send(ControlMessage::Buffer {
				pad_name: pad.name().to_string(),
				data,
				pts,
			})
			.map_err(|_| gst::FlowError::Flushing)?;

		Ok(gst::FlowSuccess::Ok)
	}

	fn forward_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
		match event.view() {
			gst::EventView::Caps(caps) => {
				let sender = match self
					.session
					.lock()
					.unwrap()
					.as_ref()
					.map(|handle| handle.sender.clone())
				{
					Some(sender) => sender,
					None => return false,
				};

				if sender
					.send(ControlMessage::SetCaps {
						pad_name: pad.name().to_string(),
						caps: caps.caps().to_owned(),
					})
					.is_err()
				{
					return false;
				}

				true
			}
			_ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
		}
	}
}

async fn run_session(settings: ResolvedSettings, mut rx: mpsc::UnboundedReceiver<ControlMessage>) -> Result<()> {
	let mut client_config = moq_native::ClientConfig::default();
	client_config.tls.disable_verify = Some(settings.tls_disable_verify);

	let client = client_config.init()?;

	let origin = moq_lite::Origin::produce();
	let mut broadcast = moq_lite::Broadcast::produce();
	let broadcast_consumer = broadcast.consume();

	let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;

	anyhow::ensure!(
		origin.publish_broadcast(&settings.broadcast, broadcast_consumer),
		"failed to publish broadcast {}",
		settings.broadcast
	);

	let client = client.with_publish(origin.consume());
	let session = client.connect(settings.url.clone()).await?;

	let mut runtime = RuntimeState {
		session,
		broadcast,
		catalog,
		pads: HashMap::new(),
	};

	while let Some(msg) = rx.recv().await {
		match msg {
			ControlMessage::SetCaps { pad_name, caps } => {
				if let Err(err) = handle_caps(&mut runtime, pad_name, caps) {
					gst::error!(CAT, "failed to configure pad: {err:#}");
				}
			}
			ControlMessage::Buffer { pad_name, data, pts } => {
				if let Err(err) = handle_buffer(&mut runtime, pad_name, data, pts) {
					gst::error!(CAT, "failed to publish buffer: {err:#}");
				}
			}
			ControlMessage::DropPad { pad_name } => {
				runtime.pads.remove(&pad_name);
			}
			ControlMessage::Shutdown => break,
		}
	}

	Ok(())
}

fn handle_caps(runtime: &mut RuntimeState, pad_name: String, caps: gst::Caps) -> Result<()> {
	let structure = caps.structure(0).context("empty caps")?;
	let decoder: moq_mux::import::Decoder = match structure.name().as_str() {
		"video/x-h264" => {
			let mut bytes = Bytes::new();
			new_decoder(runtime, moq_mux::import::DecoderFormat::Avc3, &mut bytes)?
		}
		"video/x-h265" => {
			let mut bytes = Bytes::new();
			new_decoder(runtime, moq_mux::import::DecoderFormat::Hev1, &mut bytes)?
		}
		"video/x-av1" => {
			let mut bytes = Bytes::new();
			new_decoder(runtime, moq_mux::import::DecoderFormat::Av01, &mut bytes)?
		}
		"audio/mpeg" => {
			let codec_data = structure
				.get::<gst::Buffer>("codec_data")
				.context("AAC caps missing codec_data")?;
			let map = codec_data.map_readable().context("failed to map codec_data")?;
			let mut data = Bytes::copy_from_slice(map.as_slice());
			new_decoder(runtime, moq_mux::import::DecoderFormat::Aac, &mut data)?
		}
		"audio/x-opus" => {
			let channels: i32 = structure.get("channels").unwrap_or(2);
			let rate: i32 = structure.get("rate").unwrap_or(48_000);
			let config = moq_mux::import::OpusConfig {
				sample_rate: rate as u32,
				channel_count: channels as u32,
			};
			moq_mux::import::Opus::new(runtime.broadcast.clone(), runtime.catalog.clone(), config)?.into()
		}
		other => anyhow::bail!("unsupported caps: {}", other),
	};

	runtime.pads.insert(
		pad_name,
		PadState {
			decoder,
			reference_pts: None,
		},
	);
	Ok(())
}

fn new_decoder(
	runtime: &mut RuntimeState,
	format: moq_mux::import::DecoderFormat,
	buf: &mut Bytes,
) -> Result<moq_mux::import::Decoder> {
	let decoder = moq_mux::import::Decoder::new(runtime.broadcast.clone(), runtime.catalog.clone(), format, buf)?;
	Ok(decoder)
}

fn handle_buffer(
	runtime: &mut RuntimeState,
	pad_name: String,
	mut data: Bytes,
	pts: Option<gst::ClockTime>,
) -> Result<()> {
	let pad = runtime.pads.get_mut(&pad_name).context("pad not configured")?;

	let ts = pts.and_then(|pts| {
		let reference = *pad.reference_pts.get_or_insert(pts);
		let relative = pts.checked_sub(reference)?;
		hang::container::Timestamp::from_micros(relative.nseconds() / 1000).ok()
	});

	pad.decoder.decode_frame(&mut data, ts).map_err(|e| anyhow::anyhow!(e))
}
