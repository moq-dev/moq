//! GObject shell, deliberately a parallel element (`moqsinkspike`) so production `moqsink` is untouched.

use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{Context, Result};
use bytes::Bytes;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

use super::session::{DataMsg, ResolvedSettings, SessionHandle, Status, CAT};

#[derive(Debug, Clone, Default)]
struct Settings {
	url: Option<String>,
	broadcast: Option<String>,
	tls_disable_verify: bool,
}

impl TryFrom<Settings> for ResolvedSettings {
	type Error = anyhow::Error;

	fn try_from(value: Settings) -> Result<Self> {
		Ok(Self {
			url: url::Url::parse(value.url.as_ref().context("url property is required")?)?,
			broadcast: value
				.broadcast
				.as_ref()
				.context("broadcast property is required")?
				.clone(),
			tls_disable_verify: value.tls_disable_verify,
		})
	}
}

#[derive(Default)]
pub struct MoqSinkSpike {
	settings: Mutex<Settings>,
	session: Mutex<Option<SessionHandle>>,
	status: Arc<Status>,
}

#[glib::object_subclass]
impl ObjectSubclass for MoqSinkSpike {
	const NAME: &'static str = "MoqSinkSpike";
	type Type = super::MoqSinkSpike;
	type ParentType = gst::Element;
}

impl ObjectImpl for MoqSinkSpike {
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
				// Read-only, served from the shared Status the task writes.
				glib::ParamSpecBoolean::builder("connected")
					.nick("Connected")
					.blurb("Whether the session is currently connected")
					.read_only()
					.build(),
				glib::ParamSpecString::builder("moq-version")
					.nick("Negotiated version")
					.blurb("The negotiated MoQ protocol version, null when disconnected")
					.read_only()
					.build(),
				glib::ParamSpecUInt64::builder("estimated-send-bitrate")
					.nick("Estimated send bitrate")
					.blurb("Estimated send bitrate in bits per second (congestion controller), 0 when unavailable")
					.read_only()
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
		match pspec.name() {
			"connected" => self.status.connected().to_value(),
			"moq-version" => self.status.version().to_value(),
			"estimated-send-bitrate" => self.status.send_bitrate().to_value(),
			name => {
				let settings = self.settings.lock().unwrap();
				match name {
					"url" => settings.url.to_value(),
					"broadcast" => settings.broadcast.to_value(),
					"tls-disable-verify" => settings.tls_disable_verify.to_value(),
					_ => unreachable!(),
				}
			}
		}
	}

	fn constructed(&self) {
		self.parent_constructed();
		self.obj().set_element_flags(gst::ElementFlags::SINK);
	}
}

impl GstObjectImpl for MoqSinkSpike {}

impl ElementImpl for MoqSinkSpike {
	fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
		static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
			gst::subclass::ElementMetadata::new(
				"MoQ Sink (spike)",
				"Sink/Network/MoQ",
				"Transmits media over MoQ (spike)",
				"Ariel Molina <ariel@edis.mx>",
			)
		});
		Some(&*METADATA)
	}

	fn pad_templates() -> &'static [gst::PadTemplate] {
		static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
			// Spike scope: a single H.264 pad; the point is the lifecycle, not codec breadth.
			let caps = gst::Caps::builder("video/x-h264")
				.field("stream-format", "byte-stream")
				.field("alignment", "au")
				.build();
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
				MoqSinkSpike::catch_panic_pad_function(
					parent,
					|| Err(gst::FlowError::Error),
					|sink| sink.forward_buffer(pad, buffer),
				)
			})
			.event_function(|pad, parent, event| {
				MoqSinkSpike::catch_panic_pad_function(parent, || false, |sink| sink.handle_event(pad, event))
			});

		let pad = match name {
			Some(name) => pad_builder.name(name).build(),
			None => pad_builder.generated_name().build(),
		};

		self.obj().add_pad(&pad).ok()?;
		Some(pad)
	}

	fn release_pad(&self, pad: &gst::Pad) {
		// Drop the session guard before blocking_send: holding it across a full-channel block deadlocks stop_session.
		let sender = {
			let session = self.session.lock().unwrap();
			session.as_ref().map(SessionHandle::sender)
		};
		if let Some(sender) = sender {
			let _ = sender.blocking_send(DataMsg::DropPad {
				pad: pad.name().to_string(),
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

impl MoqSinkSpike {
	fn start_session(&self) -> Result<()> {
		// Synchronous settings validation surfaces as a StateChangeError; async failures go to the bus.
		let settings = ResolvedSettings::try_from(self.settings.lock().unwrap().clone())?;
		let handle = SessionHandle::start(settings, self.status.clone(), self.obj().downgrade());
		*self.session.lock().unwrap() = Some(handle);
		Ok(())
	}

	fn stop_session(&self) {
		if let Some(handle) = self.session.lock().unwrap().take() {
			handle.stop();
		}
	}

	/// Clone the sender, release the lock, then blocking-send: never apply backpressure under the session lock.
	fn forward_buffer(&self, pad: &gst::Pad, buffer: gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
		let sender = self
			.session
			.lock()
			.unwrap()
			.as_ref()
			.map(|handle| handle.sender())
			.ok_or(gst::FlowError::Flushing)?;

		let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
		let data = Bytes::copy_from_slice(map.as_slice());
		let pts = buffer.pts();

		sender
			.blocking_send(DataMsg::Buffer {
				pad: pad.name().to_string(),
				data,
				pts,
			})
			.map_err(|_| gst::FlowError::Flushing)?;
		Ok(gst::FlowSuccess::Ok)
	}

	fn handle_event(&self, pad: &gst::Pad, event: gst::Event) -> bool {
		let sender = self.session.lock().unwrap().as_ref().map(|handle| handle.sender());

		match event.view() {
			gst::EventView::Caps(caps) => {
				let Some(sender) = sender else { return false };
				let msg = DataMsg::Caps {
					pad: pad.name().to_string(),
					caps: caps.caps().to_owned(),
				};
				if sender.blocking_send(msg).is_err() {
					return false;
				}
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Segment(segment) => {
				let Some(sender) = sender else { return false };
				let msg = DataMsg::Segment {
					pad: pad.name().to_string(),
					segment: segment.segment().to_owned(),
				};
				if sender.blocking_send(msg).is_err() {
					return false;
				}
				gst::Pad::event_default(pad, Some(&*self.obj()), event)
			}
			gst::EventView::Eos(_) => {
				let Some(sender) = sender else { return false };
				sender
					.blocking_send(DataMsg::Eos {
						pad: pad.name().to_string(),
					})
					.is_ok()
			}
			_ => gst::Pad::event_default(pad, Some(&*self.obj()), event),
		}
	}
}
