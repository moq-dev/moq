/**
 * A steering player for vehicles simulated in CARLA.
 *
 * Connects to the relay, discovers camera broadcasts by announcement (like
 * moq-boy), renders each on a map with a video preview, and lets you take
 * control of one vehicle with the keyboard or a steering wheel. The
 * {@link Truck} backend publishes a `control` track and subscribes to the
 * vehicle's `status` track for round-trip confirmation.
 *
 * @module
 */

import { Milli, Nano } from "@moq/clock";
import * as Moq from "@moq/net";
import { COLLISION_ALERT_MS, collisionAge, collisionLabel, diverged, divergence } from "./feedback.ts";
import { drawDivergence, drawDriveHud, drawGuide } from "./guides.ts";
import { controlHint } from "./hint.ts";
import { applyKeys, canShift, REST, stepDrive } from "./input.ts";
import { focusFromHash, hashWithFocus, Interaction, ownsKey } from "./interaction.ts";
import {
	expandToAspect,
	fitPoints,
	fleetTown,
	mapView,
	needsRefit,
	pathData,
	project,
	type RoadMap,
	scaleBar,
	type View,
	viewBox,
} from "./map.ts";
import { loadRoadMap } from "./maps/index.ts";
import { CAMERAS, parseCamera } from "./paths.ts";
import template from "./player.html?raw";
import { presence } from "./presence.ts";
import type { Collision, Gear, Pose, TruckStatus } from "./schemas.ts";
import style from "./style.css?inline";
import { nextGear, shiftGear, Truck } from "./truck.ts";
import {
	applyWheel,
	arbitrate,
	Calibration,
	failSafe,
	type InputSource,
	shortName,
	Wheel,
	type WheelSample,
} from "./wheel.ts";

/** Development builds at `?preview` show disconnected fixtures instead of connecting. */
export const PREVIEW = import.meta.env.DEV && new URLSearchParams(location.search).has("preview");

/**
 * Render the player into `root` and run it on `connection`.
 *
 * Everything it starts, window listeners and the input loop included, stops
 * when the returned function is called.
 */
export function mount(root: ShadowRoot, connection: Moq.Connection): () => void {
	root.innerHTML = `<style>${style}</style>${template}`;

	const interaction = new Interaction(focusFromHash(window.location.hash));
	const { driving, focused, hovered } = interaction;
	const signals = new Moq.Signals.Effect();
	// Window listeners outlive the shadow root, so they stop with the player.
	const abort = new AbortController();
	const listen = { signal: abort.signal };
	signals.cleanup(() => abort.abort());

	/** Required template nodes fail at startup instead of silently losing a control. */
	function required<T extends Element = HTMLElement>(selector: string, parent: ParentNode = root): T {
		const element = parent.querySelector<T>(selector);
		if (!element) throw new Error(`Missing UI element: ${selector}`);
		return element;
	}

	const els = {
		conn: required<HTMLSpanElement>("#conn"),
		map: required<HTMLDivElement>("#map"),
		roads: required<SVGSVGElement>("#roads"),
		markers: required<HTMLDivElement>("#markers"),
		scale: required<HTMLDivElement>("#scale"),
		note: required<HTMLDivElement>("#map-note"),
		offline: required<HTMLDivElement>("#offline"),
		backdrop: required<HTMLDivElement>("#backdrop"),
		wheel: required<HTMLButtonElement>("#wheel"),
		cal: required<HTMLDivElement>("#wheel-cal"),
		inspector: required<HTMLDivElement>("#inspector"),
	};

	els.backdrop.addEventListener("click", () => navigateFocus(undefined));

	// --- Connection status indicator. ---
	signals.run((effect) => {
		const state = effect.get(connection.status);
		els.conn.textContent = PREVIEW
			? "Local preview"
			: state === "connected"
				? "Connected"
				: state === "connecting"
					? "Connecting…"
					: "Disconnected";
		els.conn.dataset.state = PREVIEW ? "preview" : state;
	});

	// --- Discovery: maintain a Truck per announced camera broadcast. ---
	const tiles = new Map<string, TruckTile>();
	const vehicles = new Moq.Signals.Signal(0);

	signals.run((effect) => {
		// Connection retracts old announcements on disconnect, so stale trucks are removed
		// and a returning truck cannot inherit an abandoned control session.
		const announced = connection.announced(CAMERAS);
		effect.cleanup(() => announced.close());
		effect.spawn(async () => {
			for (;;) {
				const entry = await effect.race(announced.next());
				if (!entry) break;

				// A broad route can cover the scope without naming one camera.
				if (!parseCamera(entry.prefix)) continue;

				// A restart is a new simulator instance on the same path; the watch
				// broadcast follows it, so only an end removes the tile.
				if (entry.kind !== "end" && !tiles.has(entry.prefix)) {
					const truck = new Truck({ broadcastId: entry.prefix, connection, driving, focused, hovered });
					tiles.set(entry.prefix, new TruckTile(truck));
					vehicles.set(tiles.size);
					refreshFocusLayout();
				} else if (entry.kind === "end") {
					const tile = tiles.get(entry.prefix);
					tile?.dispose();
					tiles.delete(entry.prefix);
					vehicles.set(tiles.size);
					if (tile && interaction.remove(tile.truck.id)) navigateFocus(undefined);
					refreshFocusLayout();
				}
			}
		});
	});

	// --- Offline state: say why the map is empty instead of showing nothing. ---
	signals.run((effect) => {
		const state = presence(effect.get(connection.status), effect.get(vehicles));
		els.offline.hidden = state === "online";
		const [title, text] = els.offline.children;
		if (state === "offline") {
			title.textContent = "Simulator offline";
			text.replaceChildren(
				"The simulated vehicles run in CARLA on a workstation and appear here as soon as it is publishing. ",
				"Check back later, or run your own with the ",
				link("CARLA demo", "https://github.com/moq-dev/moq/tree/main/demo/carla"),
				".",
			);
		} else {
			title.textContent = "Connecting to the relay…";
			text.replaceChildren();
		}
	});

	function link(text: string, href: string): HTMLAnchorElement {
		const a = document.createElement("a");
		a.href = href;
		a.textContent = text;
		return a;
	}

	function hasTruck(id: string | undefined): boolean {
		for (const tile of tiles.values()) if (tile.truck.id === id) return true;
		return false;
	}

	// --- The map. ---
	//
	// Roads and truck markers share one coordinate system: CARLA world metres, as
	// published on `status.pose` and baked by the simulator's roadmap module. `fitted` is
	// the rectangle we want to show; `view` is that rectangle widened to the map
	// box's own aspect ratio, which is what lets an SVG viewBox in metres and
	// markers positioned in percent agree on where a truck is.
	const poses = new Map<string, Pose>();
	const towns = new Map<string, string>();
	let shownTown: string | undefined;
	let roadMap: RoadMap | undefined;
	let wantedMap: string | undefined;
	let unreadableMap: string | undefined;
	let fitted: View | undefined;
	let fittedFleet = "";
	let view: View | undefined;

	/**
	 * Adopt the fleet's town, drawing its roads if they were baked.
	 *
	 * `undefined` is a town in its own right - the fleet reporting none, because the
	 * last one left or its status lapsed. It has to drop the roads too: a pose-only
	 * truck would otherwise be projected onto the departed town's coordinates, and
	 * an empty map would keep drawing streets nobody is on.
	 */
	async function useRoadMap(name: string | undefined) {
		if (wantedMap === name) return;
		wantedMap = name;
		// Drop the old town before the load rather than after it: an await that
		// fails, or loses the race to another town, would otherwise leave the
		// previous map drawn under trucks that are somewhere else entirely.
		useRoads(undefined);
		if (name === undefined) return;

		let loaded: RoadMap | undefined;
		try {
			loaded = await loadRoadMap(name);
		} catch (err) {
			// Deliberately no retry. Status arrives at 10 Hz, so releasing the name
			// here would turn telemetry into a tight retry timer, and would re-clear
			// `fitted` on every tick. A reload is the supervisor that can actually
			// re-establish this, so say the chunk failed rather than reporting a town
			// that IS baked as if it were not.
			console.warn(`failed to load the road map for ${name}:`, err);
			if (wantedMap === name) unreadableMap = name;
			// The refit above already painted the note, back when this town still
			// looked merely unbaked. Repaint it: status snapshots dedupe, so a parked
			// fleet may never publish again to trigger one.
			scheduleRefit();
			return;
		}
		// Another town won the race while this one loaded: it owns the map now.
		if (wantedMap !== name) return;
		useRoads(loaded);
	}

	/**
	 * Adopt a town's roads, dropping the view the previous town fixed.
	 *
	 * `fitted` has to go with them. It came from the old town's bounds, or from
	 * trucks driving around inside them, and neither describes where these trucks
	 * are: without this, a fleet that lands inside the old extent leaves
	 * `needsRefit` false and gets projected against the wrong town.
	 */
	function useRoads(map: RoadMap | undefined) {
		roadMap = map;
		unreadableMap = undefined;
		fitted = undefined;
		els.roads.querySelector("#lane-geometry")?.setAttribute("d", map ? pathData(map) : "");
		const image = required<SVGImageElement>("#aerial-image");
		image.removeAttribute("href");
		els.roads.dataset.aerial = String(Boolean(map?.aerial));
		if (map?.aerial) {
			const { bbox, image: url } = map.aerial;
			image.setAttribute("x", String(bbox[0]));
			image.setAttribute("y", String(bbox[1]));
			image.setAttribute("width", String(bbox[2] - bbox[0]));
			image.setAttribute("height", String(bbox[3] - bbox[1]));
			image.setAttribute("href", url);
		}
		scheduleRefit();
	}

	let refitting = false;

	/**
	 * Coalesce refits into the next frame.
	 *
	 * Every truck republishes its pose ten times a second, and a refit measures the
	 * map before positioning markers. Doing that per pose would interleave
	 * layout reads and writes all frame; once a frame reads everything, then moves
	 * everything.
	 */
	function scheduleRefit() {
		if (refitting) return;
		refitting = true;
		requestAnimationFrame(() => {
			refitting = false;
			refitMap();
		});
	}

	/**
	 * Recompute the view and put every truck on it.
	 *
	 * A baked town fixes the view, so the map holds still while the trucks drive
	 * across it. Without one the view fits the trucks themselves, on the two
	 * triggers below.
	 */
	function refitMap() {
		// One map, one town. A relay can carry trucks from more than one CARLA world,
		// and their coordinates are unrelated, so projecting them together would
		// place someone confidently in the wrong place. Show the majority's town and
		// hide everyone else, like a truck with no position yet.
		shownTown = fleetTown(towns.values());
		void useRoadMap(shownTown);

		const points = [...poses.entries()].filter(([id]) => onShownMap(id)).map(([, pose]) => pose);
		// Two triggers, deliberately different. A truck joining or leaving is a
		// discrete event that always refits: one that left should not keep the view
		// zoomed out around where it was, and the last one leaving has to clear the
		// view rather than leave an empty extent behind. Trucks MOVING refit only
		// once one nears an edge, or the map slides under the driver at 10 Hz.
		const fleet = [...poses.keys()].filter(onShownMap).sort().join(" ");
		if (roadMap) {
			fitted = mapView(roadMap, 0.06);
		} else if (fleet !== fittedFleet || needsRefit(fitted, points)) {
			fitted = fitPoints(points);
		}
		fittedFleet = fleet;

		const box = els.map.getBoundingClientRect();
		view = fitted && box.height > 0 ? expandToAspect(fitted, box.width / box.height) : undefined;

		if (view) els.roads.setAttribute("viewBox", viewBox(view));
		els.roads.style.visibility = view && roadMap ? "visible" : "hidden";
		refreshScale();
		refreshNote();

		for (const tile of tiles.values()) tile.place();
	}

	/** Whether this truck's coordinates belong to the town the map is showing. */
	function onShownMap(id: string): boolean {
		const town = towns.get(id);
		return town === undefined || town === shownTown;
	}

	function refreshScale() {
		els.scale.hidden = !view;
		if (!view) return;
		const { metres, fraction } = scaleBar(view);
		els.scale.style.width = `${fraction * 100}%`;
		(els.scale.querySelector(".label") as HTMLElement).textContent = `${metres} m`;
	}

	let note = "";

	function code(text: string): HTMLElement {
		const el = document.createElement("code");
		el.textContent = text;
		return el;
	}

	/** Say why the map is bare, rather than leaving an empty box. */
	function refreshNote() {
		// A refit runs every frame a truck moves, so rebuild the note's nodes only
		// when what it has to say changes.
		const key = !view
			? "position"
			: roadMap
				? ""
				: unreadableMap
					? `unreadable:${unreadableMap}`
					: wantedMap
						? `unbaked:${wantedMap}`
						: "";
		if (key !== note) {
			note = key;
			if (key === "position") {
				els.note.textContent = "Waiting for a truck to report where it is…";
			} else if (key.startsWith("unreadable:")) {
				els.note.replaceChildren(
					"The road map for ",
					code(unreadableMap ?? ""),
					" did not load - reload the page to try again.",
				);
			} else if (key.startsWith("unbaked:")) {
				els.note.replaceChildren(
					"Road imagery unavailable for ",
					code(wantedMap ?? ""),
					". Showing truck positions only.",
				);
			}
		}
		// Before any truck is online, the offline state already says why.
		els.note.hidden = key === "" || (key === "position" && tiles.size === 0);
	}

	// The map's aspect ratio decides how wide the view has to be, so a resize is a
	// refit rather than just a repaint.
	const resize = new ResizeObserver(scheduleRefit);
	resize.observe(els.map);
	signals.cleanup(() => resize.disconnect());

	// --- Navigation: preview -> focus -> drive. ---

	let cameraChooser: HTMLDialogElement | undefined;

	function closeCameraChooser() {
		cameraChooser?.remove();
		cameraChooser = undefined;
	}

	/** A shared map position must still expose every camera to mouse and touch. */
	function selectMarker(tile: TruckTile) {
		closeCameraChooser();
		const pin = required<HTMLButtonElement>(".pin", tile.root);
		const bounds = pin.getBoundingClientRect();
		const neighbours = [...tiles.values()].filter((candidate) => {
			if (candidate.root.hidden) return false;
			const other = required(".pin", candidate.root).getBoundingClientRect();
			return (
				bounds.left < other.right &&
				bounds.right > other.left &&
				bounds.top < other.bottom &&
				bounds.bottom > other.top
			);
		});
		if (neighbours.length < 2) {
			navigateFocus(tile.truck.id);
			return;
		}
		const dialog = document.createElement("dialog");
		dialog.className = "camera-chooser";
		dialog.setAttribute("aria-label", "Select camera");
		for (const neighbour of neighbours) {
			const button = document.createElement("button");
			button.type = "button";
			button.textContent = neighbour.truck.id.replace("/", " · ");
			button.addEventListener("click", () => navigateFocus(neighbour.truck.id));
			dialog.appendChild(button);
		}
		const cancel = document.createElement("button");
		cancel.type = "button";
		cancel.textContent = "Cancel";
		const dismiss = () => {
			closeCameraChooser();
			if (pin.isConnected) pin.focus();
		};
		cancel.addEventListener("click", dismiss);
		dialog.addEventListener("cancel", (event) => {
			event.preventDefault();
			dismiss();
		});
		dialog.appendChild(cancel);
		root.appendChild(dialog);
		cameraChooser = dialog;
		dialog.showModal();
	}

	function navigateFocus(id: string | undefined) {
		const hash = hashWithFocus(window.location.hash, id);
		if (hash !== window.location.hash) window.location.hash = hash;
		applyFocus(id);
	}

	function applyFocus(id: string | undefined) {
		closeCameraChooser();
		if (focused.peek() !== id) clearKeys();
		interaction.focus(id);
		refreshFocusLayout();
	}

	let previousFocus: string | undefined;
	function refreshFocusLayout() {
		const id = focused.peek();
		const open = hasTruck(id);
		els.backdrop.hidden = !open;
		for (const el of root.querySelectorAll<HTMLElement>("header, main, footer")) el.inert = open;
		if (id !== previousFocus) {
			const last = previousFocus;
			previousFocus = id;
			requestAnimationFrame(() => {
				if (open) els.inspector.querySelector<HTMLButtonElement>(".close-btn")?.focus();
				else if (last) [...tiles.values()].find((tile) => tile.truck.id === last)?.focusMarker();
			});
		}
		// Keep markers aligned when the layout changes.
		scheduleRefit();
	}

	window.addEventListener("hashchange", () => applyFocus(focusFromHash(window.location.hash)), listen);

	// --- Keyboard steering for the active truck. ---
	const held = new Moq.Signals.Signal<ReadonlySet<string>>(new Set());
	const shiftBlocked = new Moq.Signals.Signal<string | undefined>(undefined);
	let shiftBlockedTimer: number | undefined;
	signals.cleanup(() => window.clearTimeout(shiftBlockedTimer));
	const KEY = (e: KeyboardEvent) => e.key.toLowerCase();
	const DRIVE_KEYS = ["w", "a", "s", "d", "c", "arrowup", "arrowdown", "arrowleft", "arrowright"];

	function holdKey(key: string) {
		if (held.peek().has(key)) return;
		held.set(new Set([...held.peek(), key]));
	}

	function releaseKey(key: string) {
		if (!held.peek().has(key)) return;
		const next = new Set(held.peek());
		next.delete(key);
		held.set(next);
	}

	function clearKeys() {
		if (held.peek().size > 0) held.set(new Set());
	}

	window.addEventListener(
		"keydown",
		(e) => {
			// The native chooser owns navigation and button activation while it is open.
			if (cameraChooser) return;
			const k = KEY(e);
			// E-stop works from anywhere on the page, calibration included: an
			// unavailable e-stop is a hazard, while a spurious one is an annoyance. It
			// claims the key only while there is a truck to stop.
			if (k === "e") {
				const truck = safetyTruck();
				if (truck) {
					truck.emergencyStop();
					e.preventDefault();
				}
				return;
			}
			// The window sees the host element as the target, so read the focused node
			// inside the shadow root instead.
			const target = e.composedPath()[0];
			if (!ownsKey(target, root)) return;
			// The calibration dialog owns the player's keys while it's up, so that
			// working the pedals to bind them can't also steer.
			if (calibration) {
				if (k === "escape") closeCalibration();
				e.preventDefault();
				return;
			}
			if (e.metaKey || e.ctrlKey || e.altKey) return;
			if (k === "tab" && hasTruck(focused.peek())) {
				const controls = [
					...els.inspector.querySelectorAll<HTMLElement>("button:not(:disabled), summary"),
				].filter((el) => el.getClientRects().length > 0);
				const first = controls[0];
				const last = controls[controls.length - 1];
				if (e.shiftKey && root.activeElement === first) {
					last?.focus();
					e.preventDefault();
				} else if (!e.shiftKey && root.activeElement === last) {
					first?.focus();
					e.preventDefault();
				}
				return;
			}
			if (k === "escape") {
				clearKeys();
				if (!interaction.releaseDrive() && focused.peek()) navigateFocus(undefined);
				e.preventDefault();
				return;
			}
			// The rest only steer a focused truck; without one, keys keep their native
			// meaning, such as Space activating a button or scrolling the host page.
			if (!focused.peek()) return;
			if (k === "g" || k === " ") {
				// Keep native Space activation for STOP. In the driving view it shifts
				// even if Take control or Close still has focus.
				if (k === " " && target instanceof HTMLElement && target.closest(".estop-btn, input, select, textarea"))
					return;
				e.preventDefault();
				if (e.repeat) return;
				const t = intentTruck();
				if (t) requestGear(t, nextGear(t.drive.peek().gear as Gear));
				return;
			}
			if (k === "r" && !e.repeat) {
				truckById(focused.peek())?.resetEstop();
				e.preventDefault();
				return;
			}
			if (DRIVE_KEYS.includes(k)) {
				intentTruck();
				holdKey(k);
				// Reaching for the keyboard takes the axes back from the wheel, which
				// otherwise holds them at wherever the rim and pedals happen to sit.
				source.set("keyboard");
				e.preventDefault();
			}
		},
		listen,
	);
	window.addEventListener("keyup", (e) => releaseKey(KEY(e)), listen);
	window.addEventListener("blur", clearKeys, listen);

	function activeTruck(): Truck | undefined {
		return truckById(driving.peek());
	}

	/** Claim the focused truck for an input that expresses driving intent. */
	function intentTruck(): Truck | undefined {
		const id = focused.peek();
		const truck = truckById(id);
		if (!id || !truck) return undefined;
		if (driving.peek() !== id && !truck.beginTakeover()) return undefined;
		if (!interaction.takeDrive(id)) return undefined;
		return truck;
	}

	function safetyTruck(): Truck | undefined {
		return truckById(interaction.safetyTarget());
	}

	function truckById(id: string | undefined): Truck | undefined {
		for (const tile of tiles.values()) if (tile.truck.id === id) return tile.truck;
		return undefined;
	}

	/** Apply the client-side half of the truck's authoritative gear interlock. */
	function requestGear(truck: Truck, gear: Gear) {
		if (!canShift(truck.drive.peek())) {
			shiftBlocked.set(truck.id);
			if (shiftBlockedTimer !== undefined) window.clearTimeout(shiftBlockedTimer);
			shiftBlockedTimer = window.setTimeout(() => shiftBlocked.set(undefined), 2000);
			return;
		}
		if (shiftBlocked.peek() === truck.id) shiftBlocked.set(undefined);
		truck.setDrive({ gear });
	}

	// --- Steering wheel (Gamepad API). ---
	//
	// Only one source drives the axes at a time. The keyboard holds them by
	// default; a connected wheel takes over once it actually moves, and only with
	// the throttle released, so a pedal resting under a foot can never be what
	// hands it control. Any drive key takes the axes straight back.
	const wheel = new Wheel();
	const source = new Moq.Signals.Signal<InputSource>("keyboard");

	/**
	 * Advance the arbitration, failing the truck safe if an armed wheel vanished.
	 *
	 * Unplugging mid-drive must not silently become a held keyboard command: the
	 * driver has lost the device that was setting throttle, so cut it here rather
	 * than letting {@link applyKeys} hold it. Fires once on the transition, so the
	 * keyboard is free to drive normally afterwards.
	 */
	function updateSource(sample: WheelSample | undefined) {
		const { source: next, lost } = arbitrate(source.peek(), sample);
		source.set(next);
		if (!lost) return;
		// A key held from before the wheel armed is still in `held`, and applyKeys
		// runs later this same frame - without this it would start easing the brake
		// back off the instant the fail-safe applied it.
		clearKeys();
		const truck = activeTruck();
		if (truck && !truck.estopped.peek()) truck.setDrive(failSafe(truck.drive.peek()));
	}

	function wheelButtons(sample: WheelSample) {
		const truck = activeTruck();
		// E-stop is the documented exception to arbitration: any source may latch it
		// at any time, on the truck in view whether or not this browser drives it.
		// Gear is a drive command, so it follows the active source - a bumped paddle
		// must not shift a truck being driven from the keyboard.
		if (sample.pressed.has("estop")) safetyTruck()?.emergencyStop();
		if (!truck || truck.estopped.peek() || source.peek() !== "wheel") return;
		const gear = truck.drive.peek().gear as Gear;
		if (sample.pressed.has("gearUp")) requestGear(truck, shiftGear(gear, 1));
		if (sample.pressed.has("gearDown")) requestGear(truck, shiftGear(gear, -1));
	}

	// Integrate the active input source into the drive state ~60 Hz. The interval
	// fallback keeps input (and therefore the control heartbeat's payload)
	// advancing when rAF is paused, e.g. in a hidden or embedded tab.
	let lastFrame = performance.now();

	function step() {
		const now = performance.now();
		const dt = Math.min((now - lastFrame) / 1000, 0.1);
		lastFrame = now;

		const sample = wheel.poll();
		refreshWheel();

		// Calibration reads the raw device, so nothing it sees should reach a truck.
		if (calibration) {
			const pad = wheel.pad();
			if (pad) calibration.update(pad);
			refreshCalibration();
			return;
		}

		// Arbitrate before reading the buttons: the gear gate below consults the
		// active source, which must already reflect this frame's sample.
		updateSource(sample);
		// Moving an armed wheel is drive intent just like pressing a drive key. The
		// arbitration throttle gate has already run, so a depressed accelerator can
		// never be the movement that claims the truck.
		if (!driving.peek() && sample?.moved && source.peek() === "wheel") intentTruck();
		if (sample) wheelButtons(sample);

		const truck = activeTruck();
		if (truck) {
			const cur = truck.drive.peek();
			const next =
				source.peek() === "wheel" && sample ? applyWheel(cur, sample, dt) : applyKeys(cur, held.peek(), dt);
			stepDrive(truck, next);
		}
	}

	let frame = requestAnimationFrame(tick);
	function tick() {
		step();
		frame = requestAnimationFrame(tick);
	}
	signals.cleanup(() => cancelAnimationFrame(frame));
	signals.interval(step, Nano.fromMilli(Milli(250)));

	// --- Wheel status chip + calibration dialog. ---
	let calibration: Calibration | undefined;
	let lastWheelLabel = "";

	const calEls = {
		device: els.cal.querySelector(".cal-device") as HTMLDivElement,
		progress: els.cal.querySelector(".cal-progress") as HTMLDivElement,
		prompt: els.cal.querySelector(".cal-prompt") as HTMLParagraphElement,
		live: els.cal.querySelector(".cal-live") as HTMLDivElement,
		warn: els.cal.querySelector(".cal-warn") as HTMLDivElement,
		capture: els.cal.querySelector(".cal-capture") as HTMLButtonElement,
		skip: els.cal.querySelector(".cal-skip") as HTMLButtonElement,
		cancel: els.cal.querySelector(".cal-cancel") as HTMLButtonElement,
		estop: els.cal.querySelector(".cal-estop") as HTMLButtonElement,
	};

	els.wheel.addEventListener("click", () => openCalibration());
	// The modal covers the tile's e-stop, so it has to carry its own.
	calEls.estop.addEventListener("click", () => safetyTruck()?.emergencyStop());
	calEls.cancel.addEventListener("click", () => closeCalibration());
	calEls.skip.addEventListener("click", () => {
		calibration?.skip();
		finishCalibration();
	});
	calEls.capture.addEventListener("click", () => {
		// A failed capture means the control didn't move far enough to tell apart
		// from the rest snapshot; leave the step up and say so.
		if (calibration && !calibration.capture()) {
			calEls.warn.hidden = false;
			return;
		}
		calEls.warn.hidden = true;
		finishCalibration();
	});

	/** Reflect the connected wheel in the header chip. */
	function refreshWheel() {
		const id = wheel.id;
		if (!id) {
			els.wheel.hidden = true;
			lastWheelLabel = "";
			return;
		}
		const label = `${shortName(id)}${wheel.calibrated ? "" : " · uncalibrated"}`;
		if (label !== lastWheelLabel) {
			els.wheel.textContent = label;
			els.wheel.title = `${id} - click to calibrate`;
			lastWheelLabel = label;
		}
		els.wheel.hidden = false;
		els.wheel.dataset.driving = String(source.peek() === "wheel");
	}

	function openCalibration() {
		if (!wheel.pad()) return;
		calibration = new Calibration();
		// The wheel has to re-arm afterwards, since the axes it reports now mean
		// something different than they did a moment ago.
		source.set("keyboard");
		clearKeys();
		// Calibration means working the pedals for reasons that have nothing to do
		// with driving, and the dialog suppresses the normal control loop, so park
		// the truck before it goes up rather than leaving whatever was commanded to
		// keep heartbeating out for the duration.
		activeTruck()?.setDrive({ ...REST });
		calEls.warn.hidden = true;
		els.cal.hidden = false;
		refreshCalibration();
	}

	function closeCalibration() {
		calibration = undefined;
		els.cal.hidden = true;
	}

	/** Save and close once the last step is bound, otherwise redraw the dialog. */
	function finishCalibration() {
		if (!calibration) return;
		if (!calibration.done) {
			refreshCalibration();
			return;
		}
		const profile = calibration.result();
		if (profile) wheel.save(profile);
		closeCalibration();
	}

	function refreshCalibration() {
		const cal = calibration;
		if (!cal) return;
		if (cal.done) {
			finishCalibration();
			return;
		}

		const step = cal.step;
		if (!step) return;

		calEls.device.textContent = wheel.id ? shortName(wheel.id) : "wheel disconnected";
		calEls.progress.textContent = `step ${cal.index + 1} of ${cal.total}`;
		calEls.prompt.textContent = step.prompt;
		// Button steps bind on the press itself - there is no free hand for Capture.
		calEls.capture.hidden = step.kind === "button";
		calEls.skip.hidden = !step.optional;

		const pad = wheel.pad();
		calEls.live.textContent = pad
			? pad.axes.map((value, index) => `a${index} ${value.toFixed(2)}`).join("  ")
			: "no signal from the wheel";
	}

	// --- A rendered tile bound to one Truck. ---
	class TruckTile {
		readonly truck: Truck;
		readonly root: HTMLDivElement;
		readonly #signals = new Moq.Signals.Effect();
		#overlay!: HTMLCanvasElement;
		#arrow!: HTMLElement;
		readonly #card: HTMLDivElement;
		readonly #panel = document.createElement("div");
		// The last collision the truck reported, held across a status dropout.
		#collision?: Collision;

		constructor(truck: Truck) {
			this.truck = truck;
			this.root = document.createElement("div");
			this.root.className = "truck";
			this.root.innerHTML = `
				<button class="pin" type="button"><span class="arrow"></span><span class="pin-label"></span></button>
				<div class="card">
					<div class="video-wrap">
						<canvas></canvas><canvas class="overlay"></canvas>
						<div class="hud-title"><div class="name"></div><span class="badge">Connecting</span></div>
						<button class="close-btn" type="button" aria-label="Back to map" title="Back to map · Esc">×</button>
						<span class="video-state">Waiting for camera</span>
						<div class="hud-pedals">
							<div class="metric"><span>Throttle</span><strong class="throttle"></strong><span class="pedal-track"><meter class="throttle-meter" min="0" max="1" aria-label="Measured throttle"></meter><i class="throttle-request request-position" aria-label="Throttle input" hidden></i></span></div>
							<div class="metric"><span>Brake</span><strong class="brake"></strong><span class="pedal-track"><meter class="brake-meter" min="0" max="1" aria-label="Measured brake"></meter><i class="brake-request request-position" aria-label="Brake input" hidden></i></span></div>
						</div>
						<div class="hud-motion"><div class="speed"><strong class="speed-value"></strong><span>km/h</span></div><strong class="gear"></strong></div>
						<div class="guide-key"><span class="actual-key">Measured</span><span class="request-key">Input</span></div>
					</div>
					<div class="body">
						<div class="name-row">
							<strong class="mode"></strong>
							<div class="actions">
								<button class="control-btn" type="button">Take control</button>
								<button class="respawn-btn" type="button" title="Return this truck to a spawn point" hidden>Reset</button>
								<button class="estop-btn" type="button" title="Emergency stop · E">E-STOP</button>
							</div>
						</div>
						<div class="telemetry">
							<div class="row collision-row" hidden><span>Collision</span><span class="collision"></span></div>
							<details class="diagnostics"><summary>Diagnostics</summary><div class="row"><span>Control round trip</span><span class="rtt"></span></div><div class="row"><span>Camera pipeline</span><span class="lat"></span></div></details>
						</div>
						<div class="hint"></div>
					</div>
				</div>`;
			this.root.hidden = true;
			els.markers.appendChild(this.root);
			this.#card = this.root.querySelector(".card") as HTMLDivElement;
			this.#panel.className = "truck";
			els.inspector.appendChild(this.#panel);

			const canvas = this.root.querySelector("canvas:not(.overlay)") as HTMLCanvasElement;
			truck.setCanvas(canvas);
			this.#overlay = this.root.querySelector("canvas.overlay") as HTMLCanvasElement;
			this.#arrow = this.root.querySelector(".pin .arrow") as HTMLElement;

			this.root.addEventListener("click", () => selectMarker(this));
			for (const target of [this.root]) {
				target.addEventListener("pointerenter", (event) => {
					if (event.pointerType !== "touch") hovered.set(truck.id);
				});
				target.addEventListener("pointerleave", () => {
					if (hovered.peek() === truck.id) hovered.set(undefined);
				});
				target.addEventListener("focusin", () => hovered.set(truck.id));
				target.addEventListener("focusout", (event) => {
					if (
						!(event.relatedTarget instanceof Node && target.contains(event.relatedTarget)) &&
						hovered.peek() === truck.id
					)
						hovered.set(undefined);
				});
			}
			required(".close-btn", this.root).addEventListener("click", (event) => {
				event.stopPropagation();
				navigateFocus(undefined);
			});

			const name = this.root.querySelector(".name");
			const control = this.root.querySelector(".control-btn") as HTMLButtonElement | null;
			const respawn = this.root.querySelector(".respawn-btn") as HTMLButtonElement | null;
			const estop = this.root.querySelector(".estop-btn");
			if (!name || !control || !respawn || !estop)
				throw new Error("truck tile template is missing required controls");
			name.textContent = truck.id.replace("/", " · ");
			required(".pin", this.root).setAttribute("aria-label", `Open ${truck.id} on map`);
			required(".pin-label", this.root).textContent = truck.id.split("/")[0];

			control.addEventListener("click", (e) => {
				e.stopPropagation();
				if (this.truck.active.peek()) {
					interaction.releaseDrive();
				} else if (this.truck.beginTakeover()) {
					interaction.takeDrive(truck.id);
				}
			});

			respawn.addEventListener("click", (e) => {
				e.stopPropagation();
				this.truck.respawn();
			});

			// E-stop is a safety control anyone may trigger, so it lives on every tile
			// and works whether or not you hold the lease. stopPropagation keeps the
			// click from also navigating into this tile's focused view.
			estop.addEventListener("click", (e) => {
				e.stopPropagation();
				this.truck.toggleEmergencyStop();
			});

			this.#bind();
		}

		/**
		 * Move this marker to the reported position, hiding it until there is one.
		 *
		 * Markers and their hover previews follow the reported position. A focused
		 * truck's card lives in the inspector, so focus works either way.
		 */
		place() {
			const pose = onShownMap(this.truck.id) ? poses.get(this.truck.id) : undefined;
			this.root.hidden = !pose || !view;
			if (!pose || !view) return;

			const { left, top } = project(view, pose);
			this.root.style.left = `${left}%`;
			this.root.style.top = `${top}%`;
			// CARLA yaw is clockwise from +x, and so is a CSS rotation drawn on a map
			// whose y grows downward, so this needs no conversion.
			this.#arrow.style.transform = `rotate(${pose.yaw}deg)`;
		}

		#bind() {
			const q = (s: string) => (s === ".card" ? this.#card : (this.#card.querySelector(s) as HTMLElement));

			// The map's inputs: where this truck is, and which town it is in.
			this.#signals.run((effect) => {
				const status = effect.get(this.truck.status);
				if (status?.map) {
					towns.set(this.truck.id, status.map);
				} else {
					towns.delete(this.truck.id);
				}
				if (status?.pose) {
					poses.set(this.truck.id, status.pose);
				} else {
					poses.delete(this.truck.id);
				}
				q(".speed-value").textContent = status?.pose ? String(Math.round(status.pose.speed * 3.6)) : "–";
				scheduleRefit();
			});

			// Preview, focus, and drive each have a distinct visual and action.
			this.#signals.run((effect) => {
				const isFocused = effect.get(focused) === this.truck.id;
				const active = effect.get(this.truck.active);
				this.root.dataset.focused = String(isFocused);
				this.root.dataset.active = String(active);
				this.#panel.dataset.focused = String(isFocused);
				this.#panel.dataset.active = String(active);
				const cardParent = isFocused ? this.#panel : this.root;
				if (this.#card.parentElement !== cardParent) cardParent.appendChild(this.#card);
				const isHovered = effect.get(hovered) === this.truck.id && !effect.get(focused);
				this.root.dataset.preview = String(isHovered);
				q(".card").hidden = !isFocused && !isHovered;
				q(".card").setAttribute("role", isFocused ? "dialog" : "region");
				q(".card").setAttribute("aria-label", this.truck.id);
				q(".card").setAttribute("aria-modal", String(isFocused));
				q(".guide-key").hidden = !active;
				const status = effect.get(this.truck.status);
				const controlButton = q(".control-btn") as HTMLButtonElement;
				controlButton.textContent = active ? "Release control" : "Take control";
				// Status carries the measured takeover state. Never guess while it is unavailable.
				controlButton.disabled = !active && !status;
				const respawnButton = q(".respawn-btn") as HTMLButtonElement;
				const driverId = effect.get(this.truck.driverId);
				const estopped = effect.get(this.truck.estopped) || Boolean(status?.estopped);
				respawnButton.hidden = !isFocused;
				respawnButton.disabled =
					effect.get(this.truck.respawning) ||
					!active ||
					!driverId ||
					status?.driver !== driverId ||
					estopped;
				// Latched if either side thinks so: the local signal reacts
				// instantly, the status echo is authoritative (see resetEstop).
				const stopButton = q(".estop-btn") as HTMLButtonElement;
				const stopParent = q(isFocused ? ".actions" : ".video-wrap");
				if (stopButton.parentElement !== stopParent) stopParent.appendChild(stopButton);
				stopButton.textContent = estopped ? "RESET E-STOP" : isFocused ? "E-STOP" : "STOP";
				stopButton.dataset.reset = String(estopped);
				// Reset waits for the authoritative echo so a fast second click cannot
				// replace the stop command before the truck receives and latches it.
				stopButton.disabled = estopped && !status?.estopped;
				stopButton.title = estopped
					? "Clear the confirmed e-stop latch and return an unclaimed truck to autonomy"
					: "Emergency stop · E";
				const statusState = effect.get(this.truck.statusState);
				q(".hint").hidden =
					!active &&
					!estopped &&
					statusState === "available" &&
					!status?.driver &&
					effect.get(shiftBlocked) !== this.truck.id;
				q(".hint").textContent =
					effect.get(shiftBlocked) === this.truck.id
						? "shift blocked - release throttle and apply the brake"
						: controlHint({
								active,
								focused: isFocused,
								estopped,
								estopBy: status?.estop_by,
								status: statusState,
								driverId,
								statusDriver: status?.driver,
								wheel: effect.get(source) === "wheel",
							});
			});

			// Camera readiness is separate from telemetry and control ownership.
			this.#signals.run((effect) => {
				const visible =
					effect.get(focused) === this.truck.id ||
					(!effect.get(focused) && effect.get(hovered) === this.truck.id);
				const frame = effect.get(this.truck.videoDecoder.out.frame);
				const stalled = effect.get(this.truck.videoDecoder.out.stalled);
				const live = visible && Boolean(frame) && !stalled;
				q(".badge").textContent = live ? "Live" : frame ? "Interrupted" : "Connecting";
				q(".video-state").textContent = frame ? "Camera interrupted" : "Waiting for camera";
				q(".badge").dataset.live = String(live);
				q(".video-state").hidden = live;
			});

			this.#signals.run((effect) => {
				const s = effect.get(this.truck.status);
				const measured = s?.measured;
				const input = effect.get(this.truck.drive);
				const active = effect.get(this.truck.active);
				for (const pedal of ["throttle", "brake"] as const) {
					q(`.${pedal}`).textContent = measured ? `${Math.round(measured[pedal] * 100)}%` : "–";
					const meter = q(`.${pedal}-meter`) as HTMLMeterElement;
					meter.hidden = !measured;
					meter.value = measured?.[pedal] ?? 0;
					q(`.${pedal}-request`).hidden = !active || !measured;
					q(`.${pedal}-request`).style.left = `${Math.max(0, Math.min(1, input[pedal])) * 100}%`;
				}
				q(".gear").textContent = measured
					? measured.gear === "forward"
						? "D"
						: measured.gear === "reverse"
							? "R"
							: "N"
					: "–";
				q(".gear").setAttribute("aria-label", measured ? `Gear: ${measured.gear}` : "Gear unavailable");
				q(".mode").textContent = !s
					? "No status"
					: s.estopped
						? "Stopped"
						: s.autonomy === "autonomous"
							? "Autonomous"
							: s.autonomy === "manual"
								? "Manual"
								: "Unknown";
				q(".mode").className = `mode${s?.estopped ? " estopped" : ""}`;
				const me = effect.get(this.truck.driverId);
				const mine = me ? s?.media_pipeline[me] : undefined;
				const roundTrip = effect.get(this.truck.roundTripMs);
				q(".rtt").textContent = roundTrip == null ? "Not measured" : `${Math.round(roundTrip)} ms`;
				q(".lat").textContent = mine?.length
					? mine.map((l) => `${l.label} ${l.ms} ms`).join(" · ")
					: "Not measured";
				this.#renderCollision(s);
			});

			// Overlay: your requested input (cyan, dashed) against the truck's
			// measured steering (white, solid), with the gap between them shaded.
			this.#signals.run((effect) => {
				effect.get(this.truck.drive);
				effect.get(this.truck.active);
				effect.get(this.truck.status);
				effect.get(focused);
				this.#drawOverlay();
			});

			// A collision's age is wall clock, not a status field, so it needs its own
			// tick: without one the callout would freeze on a truck that stops moving
			// and therefore stops changing its snapshot.
			this.#signals.interval(() => this.#renderCollision(this.truck.status.peek()), Nano.fromMilli(Milli(1000)));

			// Repaint on layout/resize (the initial effect run happens before layout,
			// so the overlay has zero size until the first observation fires).
			const ro = new ResizeObserver(() => this.#drawOverlay());
			ro.observe(this.#overlay);
			this.#signals.cleanup(() => ro.disconnect());
		}

		/**
		 * Show the truck's last collision, and call out a recent one.
		 *
		 * The age comes off the collision's own timestamp, so this holds no state
		 * beyond the last value the truck sent: every viewer ages the same impact
		 * the same way, whenever it joined, and a reload does not resurrect it as
		 * news the way locally-timed decay did.
		 *
		 * Takes the whole status because a missing collision means two different
		 * things. No status at all is a dropout - the signal goes undefined on every
		 * reconnect - and the last collision is held through it. A status that is
		 * present and omits the field is a truck that has hit nothing, including a
		 * restarted producer, and clears it.
		 */
		#renderCollision(status: TruckStatus | undefined) {
			if (status) this.#collision = status.collision;
			const q = (s: string) => (s === ".card" ? this.#card : (this.#card.querySelector(s) as HTMLElement));
			if (!this.#collision) {
				q(".collision-row").hidden = true;
				delete this.root.dataset.collision;
				delete this.#panel.dataset.collision;
				return;
			}
			const age = collisionAge(this.#collision, Date.now());
			q(".collision-row").hidden = false;
			q(".collision").textContent = collisionLabel(this.#collision, age);
			this.root.dataset.collision = age < COLLISION_ALERT_MS ? "recent" : "past";
			this.#panel.dataset.collision = this.root.dataset.collision;
		}

		/**
		 * Repaint the trajectory overlay from current signal values.
		 *
		 * Reads with `peek` rather than taking arguments because the resize observer
		 * and the effect both call it, and threading four values through both is how
		 * the two callers drift apart.
		 */
		#drawOverlay() {
			const canvas = this.#overlay;
			const cssW = canvas.clientWidth;
			const cssH = canvas.clientHeight;
			if (cssW === 0 || cssH === 0) return;

			const dpr = window.devicePixelRatio || 1;
			canvas.width = Math.round(cssW * dpr);
			canvas.height = Math.round(cssH * dpr);
			const ctx = canvas.getContext("2d");
			if (!ctx) return;
			ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
			ctx.clearRect(0, 0, cssW, cssH);

			const status = this.truck.status.peek();
			const requested = this.truck.drive.peek();
			const driving = this.truck.active.peek() && status?.autonomy !== "autonomous";
			// A marker on the map is a preview at 240p; rails on it would be noise.
			// Measured steering is worth seeing on a truck you are only watching, so
			// it is drawn whenever the panel is open, autonomous or not.
			const measured = focused.peek() === this.truck.id ? status?.measured : undefined;

			if (measured) {
				// A visible gap turns red when feedback departs from the input.
				if (driving) {
					const gap = divergence(requested, measured);
					drawDivergence(ctx, cssW, cssH, requested, measured, {
						color: diverged(gap) ? "#ff4d55" : "#ffffff",
						alpha: 0.16,
					});
				}
				drawGuide(ctx, cssW, cssH, measured, { color: "#ffffff", width: 2, alpha: 0.8 });
			}

			// Your requested input, shown while you hold control so you can see lead/lag.
			if (driving) {
				drawGuide(ctx, cssW, cssH, requested, {
					color: "#50c8ff",
					width: 2,
					dash: [7, 6],
					alpha: 0.9,
				});
			}
			// The solid pedal arrow represents measured truck feedback.
			if (measured || driving) drawDriveHud(ctx, cssW, cssH, driving ? requested : undefined, measured);
		}

		focusMarker() {
			required<HTMLButtonElement>(".pin", this.root).focus();
		}

		dispose() {
			closeCameraChooser();
			this.#signals.close();
			this.truck.close();
			this.root.remove();
			this.#panel.remove();
			if (hovered.peek() === this.truck.id) hovered.set(undefined);
			poses.delete(this.truck.id);
			towns.delete(this.truck.id);
			scheduleRefit();
		}
	}

	// A development-only, disconnected fixture for reviewing layouts without a GPU.
	if (PREVIEW) {
		required("#preview-notice").hidden = false;
		signals.spawn(async () => {
			const { populatePreview } = await import("./preview.ts");
			if (abort.signal.aborted) return;
			const cleanup = populatePreview((path) => {
				const truck = new Truck({ broadcastId: path, connection, driving, focused, hovered });
				const tile = new TruckTile(truck);
				tiles.set(path, tile);
				vehicles.set(tiles.size);
				refreshFocusLayout();
				return truck;
			});
			signals.cleanup(cleanup);
		});
	}

	function dispose() {
		for (const tile of tiles.values()) tile.dispose();
		tiles.clear();
		closeCalibration();
		signals.close();
	}

	window.addEventListener(
		"pagehide",
		(event) => {
			if (event.persisted) return;
			dispose();
			connection.close();
		},
		listen,
	);

	return dispose;
}
