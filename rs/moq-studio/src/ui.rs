//! The window: docked panels over the engine's state.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use egui::{Color32, Ui};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use moq_scene::source::Status;
use moq_scene::{Item, Rect, Scene, Source, wgpu};

use crate::engine::{Engine, State};

/// The panels, each a tab the user can move, split, or stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tab {
	Preview,
	Program,
	Scenes,
	Items,
	Sources,
	Devices,
	Audio,
	Output,
}

impl Tab {
	fn title(self) -> &'static str {
		match self {
			Self::Preview => "Preview",
			Self::Program => "Program",
			Self::Scenes => "Scenes",
			Self::Items => "Items",
			Self::Sources => "Sources",
			Self::Devices => "Devices",
			Self::Audio => "Audio",
			Self::Output => "Output",
		}
	}
}

/// The studio window.
pub struct Studio {
	dock: DockState<Tab>,
	panels: Panels,
}

impl Studio {
	pub fn new(
		cc: &eframe::CreationContext<'_>,
		engine: Arc<Engine>,
		runtime: tokio::runtime::Handle,
	) -> anyhow::Result<Self> {
		let render = cc
			.wgpu_render_state
			.as_ref()
			.context("the window is not drawn with wgpu")?;

		// The compositor's textures live as long as the engine, so registering
		// them once is enough: egui samples whatever they hold each frame.
		let (preview, program) = {
			let state = engine.state.lock().unwrap();
			let mut renderer = render.renderer.write();
			let mut register = |texture: &wgpu::Texture| {
				let view = texture.create_view(&Default::default());
				renderer.register_native_texture(&render.device, &view, wgpu::FilterMode::Linear)
			};
			(
				register(state.compositor.preview()),
				register(state.compositor.program()),
			)
		};

		let mut dock = DockState::new(vec![Tab::Preview]);
		let tree = dock.main_surface_mut();
		let [top, bottom] = tree.split_below(NodeIndex::root(), 0.62, vec![Tab::Scenes]);
		tree.split_right(top, 0.5, vec![Tab::Program]);
		let [_, items] = tree.split_right(bottom, 0.18, vec![Tab::Items]);
		let [_, sources] = tree.split_right(items, 0.4, vec![Tab::Sources, Tab::Devices]);
		tree.split_right(sources, 0.5, vec![Tab::Audio, Tab::Output]);

		Ok(Self {
			dock,
			panels: Panels {
				engine,
				runtime,
				preview,
				program,
				new_scene: String::new(),
				new_item: None,
				add: AddSource::default(),
				devices: Devices::default(),
				message: None,
			},
		})
	}
}

impl eframe::App for Studio {
	fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
		// Redraw at the canvas rate, so preview and program are live.
		let framerate = self.panels.engine.state.lock().unwrap().collection.canvas.framerate;
		ui.ctx().request_repaint_after(Duration::from_secs(1) / framerate);

		if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
			self.panels.save();
		}

		egui::Panel::top("menu").show(ui, |ui| self.panels.menu(ui));
		egui::Panel::bottom("status").show(ui, |ui| self.panels.status(ui));
		DockArea::new(&mut self.dock)
			.style(Style::from_egui(ui.style()))
			.show_inside(ui, &mut self.panels);
	}
}

/// What every panel draws from, plus the half-typed forms.
struct Panels {
	engine: Arc<Engine>,
	runtime: tokio::runtime::Handle,
	preview: egui::TextureId,
	program: egui::TextureId,
	new_scene: String,
	new_item: Option<String>,
	add: AddSource,
	devices: Devices,
	/// The outcome of the last action, shown in the status bar.
	message: Option<Result<String, String>>,
}

impl TabViewer for Panels {
	type Tab = Tab;

	fn id(&mut self, tab: &mut Tab) -> egui::Id {
		egui::Id::new(*tab)
	}

	fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
		tab.title().into()
	}

	fn is_closeable(&self, _tab: &Tab) -> bool {
		false
	}

	fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
		match tab {
			Tab::Preview => self.preview(ui),
			Tab::Program => self.program(ui),
			Tab::Scenes => self.scenes(ui),
			Tab::Items => self.items(ui),
			Tab::Sources => self.sources(ui),
			Tab::Devices => self.devices(ui),
			Tab::Audio => self.audio(ui),
			Tab::Output => self.output(ui),
		}
	}
}

impl Panels {
	fn menu(&mut self, ui: &mut Ui) {
		egui::MenuBar::new().ui(ui, |ui| {
			ui.menu_button("File", |ui| {
				if ui.button("Save").clicked() {
					self.save();
				}
				if ui.button("Reload").clicked() {
					let result = self.engine.state.lock().unwrap().reload();
					self.report(result.map(|()| "reloaded".to_string()));
				}
			});
		});
	}

	fn status(&mut self, ui: &mut Ui) {
		ui.horizontal(|ui| {
			let state = self.engine.state.lock().unwrap();
			ui.label(state.path.display().to_string());
			if state.unsaved() {
				ui.weak("(unsaved)");
			}
			drop(state);
			match &self.message {
				Some(Ok(message)) => {
					ui.separator();
					ui.label(message);
				}
				Some(Err(message)) => {
					ui.separator();
					ui.colored_label(ui.visuals().error_fg_color, message);
				}
				None => {}
			}
		});
	}

	fn save(&mut self) {
		let result = self.engine.state.lock().unwrap().save();
		self.report(result.map(|()| "saved".to_string()));
	}

	fn report(&mut self, result: anyhow::Result<String>) {
		self.message = Some(result.map_err(|err| format!("{err:#}")));
	}

	fn preview(&mut self, ui: &mut Ui) {
		let state = self.engine.state.lock().unwrap();
		let canvas = state.collection.canvas;
		ui.label(format!(
			"Preview: {}",
			state.compositor.preview_scene().unwrap_or("nothing")
		));
		drop(state);
		picture(ui, self.preview, canvas);
	}

	fn program(&mut self, ui: &mut Ui) {
		let mut state = self.engine.state.lock().unwrap();
		let canvas = state.collection.canvas;
		let fade = state.collection.transition.fade;
		ui.horizontal(|ui| {
			if ui.button("Cut").clicked() {
				state.compositor.cut();
			}
			if ui.button(format!("Fade ({} ms)", fade.as_millis())).clicked() {
				state.compositor.fade(fade, kio::clock::Instant::now());
			}
			ui.label(format!(
				"Program: {}",
				state.compositor.program_scene().unwrap_or("nothing")
			));
			if state.compositor.fading() {
				ui.weak("fading");
			}
		});
		drop(state);
		picture(ui, self.program, canvas);
	}

	fn scenes(&mut self, ui: &mut Ui) {
		let mut state = self.engine.state.lock().unwrap();
		let preview = state.compositor.preview_scene().map(str::to_owned);
		let program = state.compositor.program_scene().map(str::to_owned);
		let mut pick = None;
		let mut remove = None;

		egui::ScrollArea::vertical().show(ui, |ui| {
			for (index, scene) in state.collection.scenes.iter().enumerate() {
				ui.horizontal(|ui| {
					let selected = preview.as_deref() == Some(scene.name.as_str());
					if ui.selectable_label(selected, &scene.name).clicked() {
						pick = Some(scene.name.clone());
					}
					if program.as_deref() == Some(scene.name.as_str()) {
						ui.colored_label(Color32::RED, "on program");
					}
					if ui.small_button("Remove").clicked() {
						remove = Some(index);
					}
				});
			}
		});

		ui.separator();
		ui.horizontal(|ui| {
			ui.text_edit_singleline(&mut self.new_scene);
			if ui.button("Add scene").clicked() {
				let name = self.new_scene.trim().to_string();
				if name.is_empty() || state.collection.scene(&name).is_some() {
					self.message = Some(Err(format!("a scene needs a new, non-empty name, not `{name}`")));
				} else {
					state.edit().scenes.push(Scene::new(name.clone()));
					pick = Some(name);
					self.new_scene.clear();
				}
			}
		});

		if let Some(pick) = pick {
			state.compositor.set_preview(Some(pick));
		}
		if let Some(index) = remove {
			state.edit().scenes.remove(index);
		}
	}

	/// The items of the scene in preview, back to front.
	fn items(&mut self, ui: &mut Ui) {
		let mut state = self.engine.state.lock().unwrap();
		let Some(name) = state.compositor.preview_scene().map(str::to_owned) else {
			ui.label("Pick a scene to edit what it shows.");
			return;
		};
		let Some(scene) = state.collection.scene(&name) else {
			ui.label(format!("There is no scene `{name}`."));
			return;
		};
		let canvas = state.collection.canvas;
		let mut items = scene.items.clone();
		let mut edited = false;
		let mut moved = None;
		let mut remove = None;

		egui::ScrollArea::vertical().show(ui, |ui| {
			egui::Grid::new("items").striped(true).show(ui, |ui| {
				for heading in ["Shown", "Source", "x", "y", "Width", "Height", ""] {
					ui.strong(heading);
				}
				ui.end_row();

				let count = items.len();
				for (index, item) in items.iter_mut().enumerate() {
					edited |= ui.checkbox(&mut item.visible, "").changed();
					ui.label(&item.source);
					let rect = &mut item.rect;
					edited |= ui.add(egui::DragValue::new(&mut rect.x)).changed();
					edited |= ui.add(egui::DragValue::new(&mut rect.y)).changed();
					edited |= ui
						.add(egui::DragValue::new(&mut rect.width).range(0.0..=f32::MAX))
						.changed();
					edited |= ui
						.add(egui::DragValue::new(&mut rect.height).range(0.0..=f32::MAX))
						.changed();
					ui.horizontal(|ui| {
						if ui.small_button("Fill").clicked() {
							*rect = Rect::fill(&canvas);
							edited = true;
						}
						if ui.add_enabled(index > 0, egui::Button::new("Back").small()).clicked() {
							moved = Some((index, index - 1));
						}
						if ui
							.add_enabled(index + 1 < count, egui::Button::new("Front").small())
							.clicked()
						{
							moved = Some((index, index + 1));
						}
						if ui.small_button("Remove").clicked() {
							remove = Some(index);
						}
					});
					ui.end_row();
				}
			});

			ui.separator();
			ui.horizontal(|ui| {
				let pictures: Vec<&String> = state
					.collection
					.sources
					.iter()
					.filter(|(_, source)| source.has_video())
					.map(|(name, _)| name)
					.collect();
				// A pick whose source was removed or reloaded away would add a dangling item.
				if self.new_item.as_ref().is_some_and(|name| !pictures.contains(&name)) {
					self.new_item = None;
				}
				egui::ComboBox::from_id_salt("new item")
					.selected_text(self.new_item.as_deref().unwrap_or("Pick a source"))
					.show_ui(ui, |ui| {
						for name in pictures {
							ui.selectable_value(&mut self.new_item, Some(name.clone()), name);
						}
					});
				if ui.button("Add to scene").clicked()
					&& let Some(source) = self.new_item.take()
				{
					items.push(Item {
						source,
						rect: Rect::fill(&canvas),
						visible: true,
					});
					edited = true;
				}
			});
		});

		if let Some((from, to)) = moved {
			items.swap(from, to);
			edited = true;
		}
		if let Some(index) = remove {
			items.remove(index);
			edited = true;
		}
		if edited && let Some(scene) = state.edit().scene_mut(&name) {
			scene.items = items;
		}
	}

	fn sources(&mut self, ui: &mut Ui) {
		let engine = self.engine.clone();
		let mut state = engine.state.lock().unwrap();
		let mut remove = None;

		egui::ScrollArea::vertical().show(ui, |ui| {
			egui::Grid::new("sources").striped(true).show(ui, |ui| {
				for (name, feed) in state.sources.iter() {
					ui.label(name);
					ui.label(describe(feed.source()));
					match state.compositor.error(name) {
						Some(err) => status(ui, &Status::Failed(err.to_string())),
						None => status(ui, &feed.status()),
					}
					if ui.small_button("Remove").clicked() {
						remove = Some(name.to_string());
					}
					ui.end_row();
				}
			});

			ui.separator();
			if let Some(source) = self.add.ui(ui) {
				let name = self.add.name.trim().to_string();
				let result = add(&mut state, name, source);
				if result.is_ok() {
					self.add.name.clear();
				}
				self.report(result.map(|name| format!("added `{name}`")));
			}
		});

		if let Some(name) = remove {
			let collection = state.edit();
			collection.sources.remove(&name);
			collection.audio.remove(&name);
			for scene in &mut collection.scenes {
				scene.items.retain(|item| item.source != name);
			}
		}
	}

	fn devices(&mut self, ui: &mut Ui) {
		self.devices.poll();
		if ui.button("Refresh").clicked() {
			self.devices.refresh(&self.runtime);
		}
		let Some(picked) = self.devices.ui(ui) else { return };
		let mut state = self.engine.state.lock().unwrap();
		// Two cameras of the same model share a name; number the later ones.
		let name = std::iter::once(picked.name.clone())
			.chain((2..).map(|n| format!("{} {n}", picked.name)))
			.find(|name| !state.collection.sources.contains_key(name))
			.expect("an unused name");
		let result = add(&mut state, name, picked.source);
		drop(state);
		self.report(result.map(|name| format!("added `{name}`")));
	}

	fn audio(&mut self, ui: &mut Ui) {
		ui.horizontal(|ui| {
			ui.strong("Program");
			meter(ui, self.engine.bus.level());
		});
		ui.separator();

		let mut state = self.engine.state.lock().unwrap();
		let mut edits = Vec::new();
		egui::Grid::new("audio").striped(true).show(ui, |ui| {
			for (name, feed) in state.sources.iter() {
				let Some(strip) = feed.strip() else { continue };
				let mut audio = state.collection.audio.get(name).copied().unwrap_or_default();
				let mut db = decibels(audio.gain);
				ui.label(name);
				let mut edited = ui.checkbox(&mut audio.muted, "Mute").changed();
				if ui
					.add(egui::Slider::new(&mut db, MUTE_DB..=12.0).suffix(" dB"))
					.changed()
				{
					audio.gain = if db <= MUTE_DB { 0.0 } else { 10f32.powf(db / 20.0) };
					edited = true;
				}
				meter(ui, strip.level());
				ui.end_row();
				if edited {
					edits.push((name.to_string(), audio));
				}
			}
		});
		for (name, audio) in edits {
			state.edit().audio.insert(name, audio);
		}
	}

	fn output(&mut self, ui: &mut Ui) {
		let stats = &self.engine.stats;
		let count = |value: &std::sync::atomic::AtomicU64| value.load(Ordering::Relaxed);
		let millis = |value: &std::sync::atomic::AtomicU64| format!("{:.1} ms", count(value) as f64 / 1000.0);

		egui::Grid::new("output").show(ui, |ui| {
			match &self.engine.output {
				Some(output) => {
					ui.label("Broadcast");
					ui.label(&output.path);
					ui.end_row();
					ui.label("Relay");
					ui.label(if output.connection.connected() {
						"connected"
					} else {
						"connecting"
					});
					ui.end_row();
					ui.label("Viewers");
					ui.label(if stats.watched.load(Ordering::Relaxed) {
						"watching"
					} else {
						"none; not encoding"
					});
					ui.end_row();
				}
				None => {
					ui.label("Relay");
					ui.label("none; start with --connect to publish");
					ui.end_row();
				}
			}
			ui.label("Drawn");
			ui.label(format!("{} frames", count(&stats.rendered)));
			ui.end_row();
			ui.label("Handed to the encoder");
			ui.label(format!(
				"{} frames, last {}",
				count(&stats.copied),
				millis(&stats.copy_us)
			));
			ui.end_row();
			ui.label("Read back to the CPU");
			ui.label(if stats.zero_copy.load(Ordering::Relaxed) {
				"none; the encoder reads the GPU".to_string()
			} else {
				format!("{} frames", count(&stats.read_back))
			});
			ui.end_row();
			ui.label("Dropped");
			ui.label(format!("{} frames", count(&stats.dropped)));
			ui.end_row();
			ui.label("Encoder");
			ui.label(
				stats
					.encoder
					.lock()
					.unwrap()
					.clone()
					.unwrap_or_else(|| "not open".into()),
			);
			ui.end_row();
			ui.label("Encoded");
			ui.label(format!(
				"{} frames, last {}",
				count(&stats.encoded),
				millis(&stats.encode_us)
			));
			ui.end_row();
		});
		if let Some(error) = stats.error.lock().unwrap().as_ref() {
			ui.colored_label(ui.visuals().error_fg_color, error);
		}
	}
}

/// Add `source` as `name`, and place it over the scene in preview if it has a
/// picture. Returns the name.
fn add(state: &mut State, name: String, source: Source) -> anyhow::Result<String> {
	anyhow::ensure!(!name.is_empty(), "a source needs a name");
	anyhow::ensure!(
		!state.collection.sources.contains_key(&name),
		"there is already a source named `{name}`"
	);
	let preview = state.compositor.preview_scene().map(str::to_owned);
	let canvas = state.collection.canvas;
	let video = source.has_video();
	let collection = state.edit();
	collection.sources.insert(name.clone(), source);
	if video && let Some(scene) = preview.and_then(|preview| collection.scene_mut(&preview)) {
		scene.items.push(Item {
			source: name.clone(),
			rect: Rect::fill(&canvas),
			visible: true,
		});
	}
	Ok(name)
}

/// Draw `texture` as large as fits, keeping the canvas's shape.
fn picture(ui: &mut Ui, texture: egui::TextureId, canvas: moq_scene::Canvas) {
	let available = ui.available_size();
	let aspect = canvas.width as f32 / canvas.height as f32;
	let mut size = egui::vec2(available.x, available.x / aspect);
	if size.y > available.y {
		size = egui::vec2(available.y * aspect, available.y);
	}
	ui.vertical_centered(|ui| ui.image((texture, size)));
}

fn describe(source: &Source) -> String {
	let or_default = |id: &Option<String>| id.clone().unwrap_or_else(|| "default".into());
	match source {
		Source::Camera { id } => format!("camera: {}", or_default(id)),
		Source::Display { id } => format!("display: {}", or_default(id)),
		Source::Window { id } => format!("window: {id}"),
		Source::Microphone { id } => format!("microphone: {}", or_default(id)),
		Source::SystemAudio => "system audio".into(),
		Source::Image { path } => format!("image: {}", path.display()),
		Source::Color { color } => format!("color: {color}"),
		Source::Broadcast { path } => format!("broadcast: {path}"),
	}
}

fn status(ui: &mut Ui, status: &Status) {
	match status {
		Status::Starting => ui.weak("starting"),
		Status::Live => ui.colored_label(Color32::from_rgb(80, 200, 120), "live"),
		Status::Offline => ui.colored_label(Color32::from_rgb(220, 180, 60), "offline"),
		Status::Failed(reason) => ui
			.colored_label(ui.visuals().error_fg_color, "failed")
			.on_hover_text(reason),
		_ => ui.weak("unknown"),
	};
}

/// The quietest gain the slider offers, which it treats as silence.
const MUTE_DB: f32 = -60.0;

fn decibels(gain: f32) -> f32 {
	if gain <= 0.0 {
		MUTE_DB
	} else {
		(20.0 * gain.log10()).max(MUTE_DB)
	}
}

/// A level meter: RMS filled, peak as a tick, on a decibel scale.
fn meter(ui: &mut Ui, level: moq_audio::Level) {
	let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 12.0), egui::Sense::hover());
	let position = |amplitude: f32| (1.0 - decibels(amplitude) / MUTE_DB).clamp(0.0, 1.0);
	let painter = ui.painter_at(rect);
	painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
	let color = if level.peak() >= 1.0 {
		Color32::RED
	} else {
		Color32::from_rgb(80, 200, 120)
	};
	let mut filled = rect;
	filled.set_width(rect.width() * position(level.rms()));
	painter.rect_filled(filled, 2.0, color);
	let x = rect.left() + rect.width() * position(level.peak());
	painter.vline(x, rect.y_range(), egui::Stroke::new(2.0, color));
}

/// The form that adds a source by hand.
struct AddSource {
	name: String,
	kind: Kind,
	/// A path, broadcast path, or device id, depending on the kind.
	value: String,
	color: Color32,
}

impl Default for AddSource {
	fn default() -> Self {
		Self {
			name: String::new(),
			kind: Kind::default(),
			value: String::new(),
			color: Color32::WHITE,
		}
	}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Kind {
	#[default]
	Color,
	Image,
	Broadcast,
	Camera,
	Display,
	Window,
	Microphone,
	SystemAudio,
}

impl Kind {
	const ALL: [Self; 8] = [
		Self::Color,
		Self::Image,
		Self::Broadcast,
		Self::Camera,
		Self::Display,
		Self::Window,
		Self::Microphone,
		Self::SystemAudio,
	];

	fn label(self) -> &'static str {
		match self {
			Self::Color => "Color",
			Self::Image => "Image",
			Self::Broadcast => "Broadcast",
			Self::Camera => "Camera",
			Self::Display => "Display",
			Self::Window => "Window",
			Self::Microphone => "Microphone",
			Self::SystemAudio => "System audio",
		}
	}

	/// What the value field holds, if this kind takes one.
	fn value(self) -> Option<&'static str> {
		match self {
			Self::Color | Self::SystemAudio => None,
			Self::Image => Some("file"),
			Self::Broadcast => Some("path"),
			Self::Window => Some("window id"),
			Self::Camera | Self::Display | Self::Microphone => Some("id (empty for the default)"),
		}
	}
}

impl AddSource {
	/// Draw the form; the source to add once the user asks for it.
	fn ui(&mut self, ui: &mut Ui) -> Option<Source> {
		let mut add = false;
		egui::Grid::new("add source").show(ui, |ui| {
			ui.label("Name");
			ui.text_edit_singleline(&mut self.name);
			ui.end_row();
			ui.label("Kind");
			egui::ComboBox::from_id_salt("kind")
				.selected_text(self.kind.label())
				.show_ui(ui, |ui| {
					for kind in Kind::ALL {
						ui.selectable_value(&mut self.kind, kind, kind.label());
					}
				});
			ui.end_row();
			if let Some(label) = self.kind.value() {
				ui.label(label);
				ui.text_edit_singleline(&mut self.value);
				ui.end_row();
			}
			if self.kind == Kind::Color {
				ui.label("Color");
				egui::color_picker::color_edit_button_srgba(ui, &mut self.color, egui::color_picker::Alpha::OnlyBlend);
				ui.end_row();
			}
			add = ui.button("Add source").clicked();
			ui.end_row();
		});
		if !add {
			return None;
		}

		let value = self.value.trim().to_string();
		let id = (!value.is_empty()).then(|| value.clone());
		Some(match self.kind {
			Kind::Color => Source::Color {
				color: moq_scene::Rgba(self.color.to_srgba_unmultiplied()),
			},
			Kind::Image => Source::Image { path: value.into() },
			Kind::Broadcast => Source::Broadcast { path: value },
			Kind::Camera => Source::Camera { id },
			Kind::Display => Source::Display { id },
			Kind::Window => Source::Window { id: value },
			Kind::Microphone => Source::Microphone { id },
			Kind::SystemAudio => Source::SystemAudio,
		})
	}
}

/// A device the user can add as a source.
struct Pick {
	name: String,
	source: Source,
}

/// The capture devices this machine has, listed in the background.
#[derive(Default)]
struct Devices {
	listing: Option<Listing>,
	pending: Option<std::sync::mpsc::Receiver<Listing>>,
}

/// One listing of every kind of device, each of which can fail on its own.
struct Listing {
	kinds: Vec<(&'static str, Result<Vec<Pick>, String>)>,
}

impl Devices {
	fn refresh(&mut self, runtime: &tokio::runtime::Handle) {
		let (send, recv) = std::sync::mpsc::channel();
		self.pending = Some(recv);
		runtime.spawn(async move {
			let _ = send.send(list().await);
		});
	}

	fn poll(&mut self) {
		if let Some(listing) = self.pending.as_ref().and_then(|pending| pending.try_recv().ok()) {
			self.listing = Some(listing);
			self.pending = None;
		}
	}

	/// Draw the listing; the device to add once the user picks one.
	fn ui(&mut self, ui: &mut Ui) -> Option<Pick> {
		if self.pending.is_some() {
			ui.spinner();
		}
		let Some(listing) = &mut self.listing else {
			ui.label("Refresh to list this machine's cameras, screens, and inputs.");
			return None;
		};
		let mut picked = None;
		egui::ScrollArea::vertical()
			.show(ui, |ui| {
				for (kind, devices) in &mut listing.kinds {
					ui.strong(*kind);
					match devices {
						Ok(devices) if devices.is_empty() => {
							ui.weak("none");
						}
						Ok(devices) => {
							for (index, device) in devices.iter().enumerate() {
								ui.horizontal(|ui| {
									if ui.small_button("Add").clicked() {
										picked = Some(index);
									}
									ui.label(&device.name);
								});
							}
							if let Some(index) = picked.take() {
								return Some(devices.remove(index));
							}
						}
						Err(err) => {
							ui.colored_label(ui.visuals().error_fg_color, err.as_str());
						}
					}
				}
				None
			})
			.inner
	}
}

#[cfg(feature = "capture")]
async fn list() -> Listing {
	fn picks<T>(result: Result<Vec<T>, impl std::fmt::Display>, pick: impl Fn(T) -> Pick) -> Result<Vec<Pick>, String> {
		result
			.map(|devices| devices.into_iter().map(pick).collect())
			.map_err(|err| err.to_string())
	}

	let cameras = picks(moq_video::capture::cameras().await, |camera| Pick {
		name: camera.name,
		source: Source::Camera { id: Some(camera.id) },
	});
	let displays = picks(moq_video::capture::displays().await, |display| Pick {
		name: display.name,
		source: Source::Display { id: Some(display.id) },
	});
	let windows = picks(moq_video::capture::windows().await, |window| Pick {
		name: format!("{} ({})", window.title, window.app),
		source: Source::Window { id: window.id },
	});
	let mut microphones = picks(moq_audio::capture::devices().await, |device| Pick {
		name: format!("{} ({})", device.name, device.host),
		source: Source::Microphone { id: Some(device.id) },
	});
	if cfg!(target_os = "macos")
		&& let Ok(microphones) = &mut microphones
	{
		microphones.push(Pick {
			name: "System audio".into(),
			source: Source::SystemAudio,
		});
	}

	Listing {
		kinds: vec![
			("Cameras", cameras),
			("Displays", displays),
			("Windows", windows),
			("Audio inputs", microphones),
		],
	}
}

#[cfg(not(feature = "capture"))]
async fn list() -> Listing {
	Listing {
		kinds: vec![(
			"Devices",
			Err("this build cannot capture; rebuild with `--features capture`".into()),
		)],
	}
}
