//! The winit event loop: one window, one wgpu surface, and the presentation
//! schedule that decides when a decoded frame reaches it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use hang::moq_net;
use moq_video::render::wgpu;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use super::args::Args;
use super::layout::fit;
use super::media::Media;
use super::timeline::{Clock, timestamp};

/// How early a frame may be shown rather than waiting another wakeup for it.
/// Under a display's frame interval, so it can't be seen, but enough that timer
/// slop doesn't push every frame a whole refresh late.
const VIDEO_EARLY_TOLERANCE: Duration = Duration::from_millis(2);

/// Consecutive surface rebuilds before a frame is written off. A display change
/// or a resume costs one; without a ceiling, a surface that can never present
/// would redraw forever, since each retry is what schedules the next.
const MAX_PRESENT_RETRIES: u32 = 8;

/// What the media, transport, and signal tasks tell the event loop.
pub(super) enum Event {
	/// New media is queued; redraw.
	Wake,
	/// Stop now: Ctrl-C, or the transport is gone and there is nothing more coming.
	Finished,
	/// Every track reached its end. Present what is still queued, then stop.
	Ended,
	Failed(String),
}

/// Run native playback on the calling thread until the window closes.
///
/// Blocking is deliberate: winit only builds an event loop on the process main
/// thread, so this has to stay on the `#[tokio::main]` future rather than being
/// spawned. Media and transport run on tasks and talk to it through the proxy.
pub fn run(
	origin: moq_net::origin::Consumer,
	broadcast: String,
	args: Args,
	network: tokio::task::JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<()> {
	let event_loop = EventLoop::<Event>::with_user_event()
		.build()
		.context("failed to create the playback event loop")?;
	let proxy = event_loop.create_proxy();
	let video = Arc::new(Mutex::new(VecDeque::new()));
	let audio_clock = Arc::new(Mutex::new(None));
	// Signals the decoder that the presenter took a frame, so it can hand over
	// the next one instead of dropping it.
	let drained = Arc::new(tokio::sync::Notify::new());

	let media = tokio::spawn(
		Media {
			origin,
			broadcast: broadcast.clone(),
			args,
			video: video.clone(),
			audio_clock: audio_clock.clone(),
			drained: drained.clone(),
			proxy: proxy.clone(),
		}
		.run(),
	);
	let network = tokio::spawn(watch_network(network, proxy.clone()));
	let signal = tokio::spawn({
		let proxy = proxy.clone();
		async move {
			if tokio::signal::ctrl_c().await.is_ok() {
				let _ = proxy.send_event(Event::Finished);
			}
		}
	});

	let title = if broadcast.is_empty() {
		"moq play".to_string()
	} else {
		format!("moq play: {broadcast}")
	};
	let mut app = App::new(title, video, audio_clock, drained);
	let result = event_loop.run_app(&mut app).context("playback event loop failed");
	media.abort();
	network.abort();
	signal.abort();
	result?;

	match app.error {
		Some(err) => anyhow::bail!(err),
		None => Ok(()),
	}
}

async fn watch_network(mut tasks: tokio::task::JoinSet<anyhow::Result<()>>, proxy: EventLoopProxy<Event>) {
	while let Some(result) = tasks.join_next().await {
		let event = match result {
			Ok(Ok(())) => Event::Finished,
			Ok(Err(err)) => Event::Failed(format!("MoQ transport failed: {err:#}")),
			Err(err) if err.is_cancelled() => continue,
			Err(err) => Event::Failed(format!("MoQ transport task failed: {err}")),
		};
		let _ = proxy.send_event(event);
		return;
	}
}

struct App {
	title: String,
	video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
	audio_clock: Arc<Mutex<Option<Clock>>>,
	drained: Arc<tokio::sync::Notify>,
	video_clock: Option<Clock>,
	display: Option<Display>,
	next_redraw: Option<Instant>,
	/// The media tasks are done. Keep presenting whatever they left queued, then
	/// stop; exiting the moment the decoder hits EOF would cut the tail off.
	ending: bool,
	/// Consecutive presents that rebuilt the surface instead of showing a frame.
	retries: u32,
	error: Option<String>,
}

impl App {
	fn new(
		title: String,
		video: Arc<Mutex<VecDeque<moq_video::Frame>>>,
		audio_clock: Arc<Mutex<Option<Clock>>>,
		drained: Arc<tokio::sync::Notify>,
	) -> Self {
		Self {
			title,
			video,
			audio_clock,
			drained,
			video_clock: None,
			display: None,
			next_redraw: None,
			ending: false,
			retries: 0,
			error: None,
		}
	}

	fn redraw(&mut self) -> anyhow::Result<()> {
		let Some(display) = self.display.as_mut() else {
			return Ok(());
		};
		let mut video = self.video.lock().unwrap();
		let audio_clock = *self.audio_clock.lock().unwrap();

		if self.video_clock.is_none()
			&& audio_clock.is_none()
			&& let Some(frame) = video.front()
		{
			self.video_clock = Some(Clock {
				media: timestamp(frame.timestamp),
				wall: Instant::now(),
			});
		}
		let clock = audio_clock.or(self.video_clock);
		let now = clock.map(Clock::now);
		let mut due = None;
		while video.front().is_some_and(|frame| {
			now.is_none_or(|now| timestamp(frame.timestamp) <= now.saturating_add(VIDEO_EARLY_TOLERANCE))
		}) {
			due = video.pop_front();
		}
		let next_timestamp = video.front().map(|frame| timestamp(frame.timestamp));
		let popped = due.is_some();
		drop(video);

		// Room in the queue, so the decoder can hand over whatever it held back.
		if popped {
			self.drained.notify_one();
		}

		if let Some(frame) = due {
			display.render(&frame)?;
		}
		let presented = display.present()?;

		// `checked_add`: the wait comes from a wire timestamp, and adding a bogus
		// one to an `Instant` panics rather than saturating. No deadline just means
		// the next frame waits for a media wakeup instead.
		self.next_redraw = match (clock, next_timestamp) {
			(Some(clock), Some(next)) => Instant::now().checked_add(next.saturating_sub(clock.now())),
			_ => None,
		};

		// A rebuilt surface still owes us the frame we just drew, and nothing else
		// will ask for it: a stalled live stream has no next frame to trigger one,
		// and an ending stream would exit first. Bounded, so a surface that can
		// never present fails instead of looping.
		match presented {
			Presented::Shown => self.retries = 0,
			// Giving up is a dropped frame, not a failure: the next one redraws.
			// Retrying without a budget would spin, since each retry asks for the
			// redraw that produces the next one.
			Presented::Retry if self.retries >= MAX_PRESENT_RETRIES => {
				tracing::warn!("gave up re-presenting after rebuilding the graphics surface");
				self.retries = 0;
			}
			Presented::Retry => {
				self.retries += 1;
				display.window.request_redraw();
			}
		}
		Ok(())
	}

	fn fail(&mut self, event_loop: &ActiveEventLoop, err: impl ToString) {
		self.error = Some(err.to_string());
		event_loop.exit();
	}
}

impl ApplicationHandler<Event> for App {
	fn resumed(&mut self, event_loop: &ActiveEventLoop) {
		if self.display.is_some() {
			return;
		}
		match Display::new(event_loop, &self.title) {
			// Draw once up front so the window is black while the broadcast is
			// still resolving, rather than showing whatever was in the surface.
			Ok(display) => {
				display.window.request_redraw();
				self.display = Some(display);
			}
			Err(err) => self.fail(event_loop, err),
		}
	}

	fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
		match event {
			Event::Wake => {
				if let Some(display) = &self.display {
					display.window.request_redraw();
				}
			}
			Event::Finished => event_loop.exit(),
			Event::Ended => {
				self.ending = true;
				if let Some(display) = &self.display {
					display.window.request_redraw();
				}
			}
			Event::Failed(err) => self.fail(event_loop, err),
		}
	}

	fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
		if self
			.display
			.as_ref()
			.is_none_or(|display| display.window.id() != window_id)
		{
			return;
		}
		match event {
			WindowEvent::CloseRequested
			| WindowEvent::KeyboardInput {
				event:
					KeyEvent {
						logical_key: Key::Named(NamedKey::Escape),
						state: ElementState::Pressed,
						..
					},
				..
			} => event_loop.exit(),
			WindowEvent::Resized(size) => {
				if let Some(display) = self.display.as_mut() {
					display.resize(size.width, size.height);
					display.window.request_redraw();
				}
			}
			WindowEvent::RedrawRequested => {
				if let Err(err) = self.redraw() {
					self.fail(event_loop, err);
				}
			}
			_ => {}
		}
	}

	fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
		// Nothing left to decode and nothing left to show. No window means nothing
		// can drain the queue, so don't wait on it.
		let drained = self.display.is_none() || self.video.lock().unwrap().is_empty();
		if self.ending && self.retries == 0 && self.next_redraw.is_none() && drained {
			event_loop.exit();
			return;
		}

		match self.next_redraw {
			Some(deadline) if deadline <= Instant::now() => {
				self.next_redraw = None;
				if let Some(display) = &self.display {
					display.window.request_redraw();
				}
			}
			Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
			None => event_loop.set_control_flow(ControlFlow::Wait),
		}
	}
}

struct Display {
	window: Arc<Window>,
	/// Kept past setup so a lost surface can be rebuilt from it.
	instance: wgpu::Instance,
	surface: wgpu::Surface<'static>,
	device: wgpu::Device,
	queue: wgpu::Queue,
	config: wgpu::SurfaceConfiguration,
	renderer: moq_video::render::Renderer,
	presenter: Presenter,
	texture: Option<(wgpu::Texture, moq_video::Size)>,
}

impl Display {
	fn new(event_loop: &ActiveEventLoop, title: &str) -> anyhow::Result<Self> {
		let window = Arc::new(
			event_loop.create_window(
				Window::default_attributes()
					.with_title(title)
					.with_inner_size(LogicalSize::new(960, 540)),
			)?,
		);
		let size = window.inner_size();
		let instance = wgpu::Instance::default();
		let surface = instance.create_surface(window.clone())?;
		let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
			power_preference: wgpu::PowerPreference::HighPerformance,
			force_fallback_adapter: false,
			compatible_surface: Some(&surface),
			apply_limit_buckets: false,
		}))?;
		let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
		let mut config = surface
			.get_default_config(&adapter, size.width.max(1), size.height.max(1))
			.context("the graphics adapter cannot present to this window")?;
		let caps = surface.get_capabilities(&adapter);
		if let Some(format) = caps.formats.iter().copied().find(|format| !format.is_srgb()) {
			config.format = format;
		}
		config.desired_maximum_frame_latency = 1;
		surface.configure(&device, &config);
		let renderer = moq_video::render::Renderer::new(&device, &queue, Default::default())?;
		let presenter = Presenter::new(&device, config.format);

		Ok(Self {
			window,
			instance,
			surface,
			device,
			queue,
			config,
			renderer,
			presenter,
			texture: None,
		})
	}

	fn resize(&mut self, width: u32, height: u32) {
		if width == 0 || height == 0 {
			return;
		}
		self.config.width = width;
		self.config.height = height;
		self.surface.configure(&self.device, &self.config);
	}

	fn render(&mut self, frame: &moq_video::Frame) -> anyhow::Result<()> {
		self.texture = Some((self.renderer.render(frame)?, frame.size()));
		Ok(())
	}

	fn present(&mut self) -> anyhow::Result<Presented> {
		use wgpu::CurrentSurfaceTexture;

		let (output, reconfigure) = match self.surface.get_current_texture() {
			CurrentSurfaceTexture::Success(output) => (output, false),
			CurrentSurfaceTexture::Suboptimal(output) => (output, true),
			// The swapchain was busy, not broken. Ask again rather than waiting on a
			// next frame that a stalled or ending stream may never produce.
			CurrentSurfaceTexture::Timeout => return Ok(Presented::Retry),
			// Nobody is looking, so there is nothing to retry for. Being shown again
			// is itself a redraw.
			CurrentSurfaceTexture::Occluded => return Ok(Presented::Shown),
			CurrentSurfaceTexture::Outdated => {
				self.surface.configure(&self.device, &self.config);
				return Ok(Presented::Retry);
			}
			// Not just a stale configuration like `Outdated`: the surface itself is
			// gone and has to be rebuilt before it can be configured again. A
			// display change or a resume can do this, so it isn't fatal.
			CurrentSurfaceTexture::Lost => {
				self.surface = self
					.instance
					.create_surface(self.window.clone())
					.context("failed to rebuild the playback window's graphics surface")?;
				self.surface.configure(&self.device, &self.config);
				return Ok(Presented::Retry);
			}
			CurrentSurfaceTexture::Validation => anyhow::bail!("failed to acquire the playback window's next frame"),
		};
		let target = output.texture.create_view(&Default::default());
		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq play present"),
		});
		let viewport = self
			.texture
			.as_ref()
			.map(|(_, size)| fit((self.config.width, self.config.height), (size.width, size.height)));
		self.presenter.draw(
			&self.device,
			&mut encoder,
			self.texture.as_ref().map(|(texture, _)| texture),
			&target,
			viewport,
		);
		self.queue.submit([encoder.finish()]);
		self.queue.present(output);
		if reconfigure {
			self.surface.configure(&self.device, &self.config);
		}
		Ok(Presented::Shown)
	}
}

/// Whether a present reached the screen, or recovered the surface and still owes
/// the caller a redraw.
#[derive(Clone, Copy)]
enum Presented {
	Shown,
	Retry,
}

struct Presenter {
	pipeline: wgpu::RenderPipeline,
	layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
}

impl Presenter {
	fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq play texture layout"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: true },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
					count: None,
				},
			],
		});
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("moq play pipeline layout"),
			bind_group_layouts: &[Some(&layout)],
			immediate_size: 0,
		});
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq play shader"),
			source: wgpu::ShaderSource::Wgsl(
				"struct VertexOutput {\n\
				   @builtin(position) position: vec4f,\n\
				   @location(0) tex_coords: vec2f,\n\
				 }\n\
				 @group(0) @binding(0) var image: texture_2d<f32>;\n\
				 @group(0) @binding(1) var image_sampler: sampler;\n\
				 @vertex fn vs_main(@builtin(vertex_index) i: u32) -> VertexOutput {\n\
				   var out: VertexOutput;\n\
				   out.tex_coords = vec2f(f32((i << 1u) & 2u), f32(i & 2u));\n\
				   out.position = vec4f(out.tex_coords * 2.0 - 1.0, 0.0, 1.0);\n\
				   out.tex_coords.y = 1.0 - out.tex_coords.y;\n\
				   return out;\n\
				 }\n\
				 @fragment fn fs_main(in: VertexOutput) -> @location(0) vec4f {\n\
				   return textureSample(image, image_sampler, in.tex_coords);\n\
				 }"
				.into(),
			),
		});
		let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("moq play pipeline"),
			layout: Some(&pipeline_layout),
			vertex: wgpu::VertexState {
				module: &shader,
				entry_point: Some("vs_main"),
				compilation_options: Default::default(),
				buffers: &[],
			},
			primitive: Default::default(),
			depth_stencil: None,
			multisample: Default::default(),
			fragment: Some(wgpu::FragmentState {
				module: &shader,
				entry_point: Some("fs_main"),
				compilation_options: Default::default(),
				targets: &[Some(wgpu::ColorTargetState {
					format,
					blend: None,
					write_mask: wgpu::ColorWrites::ALL,
				})],
			}),
			multiview_mask: None,
			cache: None,
		});
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("moq play sampler"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			..Default::default()
		});
		Self {
			pipeline,
			layout,
			sampler,
		}
	}

	fn draw(
		&self,
		device: &wgpu::Device,
		encoder: &mut wgpu::CommandEncoder,
		source: Option<&wgpu::Texture>,
		target: &wgpu::TextureView,
		viewport: Option<(f32, f32, f32, f32)>,
	) {
		let bind = source.map(|texture| {
			let view = texture.create_view(&Default::default());
			device.create_bind_group(&wgpu::BindGroupDescriptor {
				label: Some("moq play texture"),
				layout: &self.layout,
				entries: &[
					wgpu::BindGroupEntry {
						binding: 0,
						resource: wgpu::BindingResource::TextureView(&view),
					},
					wgpu::BindGroupEntry {
						binding: 1,
						resource: wgpu::BindingResource::Sampler(&self.sampler),
					},
				],
			})
		});
		let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
			label: Some("moq play present"),
			color_attachments: &[Some(wgpu::RenderPassColorAttachment {
				view: target,
				depth_slice: None,
				resolve_target: None,
				ops: wgpu::Operations {
					load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
					store: wgpu::StoreOp::Store,
				},
			})],
			depth_stencil_attachment: None,
			timestamp_writes: None,
			occlusion_query_set: None,
			multiview_mask: None,
		});
		if let (Some(bind), Some((x, y, width, height))) = (bind.as_ref(), viewport) {
			pass.set_viewport(x, y, width, height, 0.0, 1.0);
			pass.set_pipeline(&self.pipeline);
			pass.set_bind_group(0, bind, &[]);
			pass.draw(0..3, 0..1);
		}
	}
}
