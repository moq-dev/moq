import * as Moq from "@moq/lite";
import * as Watch from "@moq/watch";

// Parse URL params.
const params = new URLSearchParams(window.location.search);
const url = new URL(params.get("url") ?? import.meta.env.VITE_RELAY_URL ?? "https://cdn.moq.dev/anon");

const statusEl = document.getElementById("connection-status") as HTMLElement;
const gridEl = document.getElementById("grid") as HTMLElement;
const emptyState = document.getElementById("empty-state") as HTMLElement;

function updateEmptyState() {
	emptyState.style.display = drones.size === 0 ? "block" : "none";
}

// Single shared connection for everything.
const connection = new Moq.Connection.Reload({ url, enabled: true });

// Track connection status.
const root = new Moq.Signals.Effect();
root.run((e) => {
	const status = e.get(connection.status);
	statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
	statusEl.style.color = status === "connected" ? "#4ade80" : status === "connecting" ? "#facc15" : "#888";
});

// Track which drone is expanded (fullscreen).
const expanded = new Moq.Signals.Signal<string | undefined>(undefined);

// Active drone cards.
const drones = new Map<string, DroneCard>();
updateEmptyState();

// Discover drones via announcements.
root.run((effect) => {
	const conn = effect.get(connection.established);
	if (!conn) return;

	const announced = conn.announced(Moq.Path.from("drone"));
	effect.cleanup(() => announced.close());

	effect.spawn(async () => {
		for (;;) {
			const entry = await Promise.race([effect.cancel, announced.next()]);
			if (!entry) break;

			// Strip the "drone/" prefix to get the drone ID.
			// Skip nested paths like "drone/abc123/viewer/..."
			const suffix = Moq.Path.stripPrefix(Moq.Path.from("drone"), entry.path);
			if (!suffix || suffix.includes("/")) continue;

			const id = suffix;
			if (entry.active && !drones.has(id)) {
				const card = new DroneCard(id);
				drones.set(id, card);
				gridEl.appendChild(card.el);
				updateEmptyState();
			} else if (!entry.active) {
				const card = drones.get(id);
				if (card) {
					card.close();
					card.el.remove();
					drones.delete(id);
					updateEmptyState();
				}
			}
		}
	});
});

interface DroneStatus {
	actions: string[];
	controllers: string[];
}

// A drone card: live video + sensor HUD, expandable to fullscreen with controls.
class DroneCard {
	el: HTMLDivElement;
	#signals = new Moq.Signals.Effect();
	#sendCommand: (cmd: unknown) => void = () => {};

	constructor(droneId: string) {
		this.el = document.createElement("div");
		this.el.className = "card";

		// Canvas for video.
		const canvas = document.createElement("canvas");
		canvas.className = "video";
		this.el.appendChild(canvas);

		// Label overlay.
		const label = document.createElement("div");
		label.className = "label";
		label.textContent = droneId;
		this.el.appendChild(label);

		// Sensor HUD overlay.
		const hud = document.createElement("div");
		hud.className = "hud";
		hud.textContent = "...";
		this.el.appendChild(hud);

		// Controller alert overlay.
		const alert = document.createElement("div");
		alert.className = "alert";
		this.el.appendChild(alert);

		// Controls container (visible when expanded).
		const controls = document.createElement("div");
		controls.className = "controls";
		this.el.appendChild(controls);

		// Build the control layout.
		const controlsInner = this.buildControls();
		controls.appendChild(controlsInner);

		// Click to toggle expand (but not on controls).
		canvas.addEventListener("click", () => {
			expanded.set(expanded.peek() === droneId ? undefined : droneId);
		});

		// React to expand state for styling.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const isExpanded = exp === droneId;
			this.el.classList.toggle("expanded", isExpanded);
			controls.style.display = isExpanded ? "flex" : "none";
		});

		// Keyboard input when expanded.
		const keyHandler = (e: KeyboardEvent) => {
			if (expanded.peek() !== droneId) return;

			switch (e.key) {
				case "ArrowLeft":
					this.#sendCommand({ type: "action", name: "left" });
					e.preventDefault();
					break;
				case "ArrowRight":
					this.#sendCommand({ type: "action", name: "right" });
					e.preventDefault();
					break;
				case "ArrowUp":
					this.#sendCommand({ type: "action", name: "up" });
					e.preventDefault();
					break;
				case "ArrowDown":
					this.#sendCommand({ type: "action", name: "down" });
					e.preventDefault();
					break;
				case " ":
					this.#sendCommand({ type: "action", name: "grab" });
					e.preventDefault();
					break;
				case "Escape":
					expanded.set(undefined);
					e.preventDefault();
					break;
			}
		};
		document.addEventListener("keydown", keyHandler);
		this.#signals.cleanup(() => document.removeEventListener("keydown", keyHandler));

		// Set up video via Watch API, sharing the connection.
		const broadcast = new Watch.Broadcast({
			connection: connection.established,
			name: Moq.Path.from(`drone/${droneId}`),
			enabled: true,
		});
		this.#signals.cleanup(() => broadcast.close());

		const sync = new Watch.Sync();
		this.#signals.cleanup(() => sync.close());

		const videoSource = new Watch.Video.Source(sync, { broadcast });
		this.#signals.cleanup(() => videoSource.close());

		// Set pixel budget based on expanded state.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const pixels = exp === droneId ? 1920 * 1080 : 478 * 360;
			videoSource.target.set({ pixels });
		});

		const videoDecoder = new Watch.Video.Decoder(videoSource);
		this.#signals.cleanup(() => videoDecoder.close());

		// Disable non-expanded cards when one is expanded to save bandwidth.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const active = exp === undefined || exp === droneId;
			videoDecoder.enabled.set(active);
		});

		const videoRenderer = new Watch.Video.Renderer(videoDecoder, { canvas });
		this.#signals.cleanup(() => videoRenderer.close());

		// Subscribe to raw sensor track for HUD.
		this.#signals.run((effect) => {
			const active = effect.get(broadcast.active);
			if (!active) return;

			const sensorTrack = active.subscribe("sensor", 10);
			effect.cleanup(() => sensorTrack.close());

			effect.spawn(async () => {
				for (;;) {
					const json = (await Promise.race([effect.cancel, sensorTrack.readJson()])) as
						| { battery: number; temp: number; gps: [number, number]; uptime: number }
						| undefined;
					if (!json) break;
					hud.textContent = `BAT ${json.battery}% | ${json.temp.toFixed(1)}°C | UP ${formatTime(json.uptime)}`;
				}
			});
		});

		// Track status from status track.
		const status = new Moq.Signals.Signal<DroneStatus | undefined>(undefined);

		this.#signals.run((effect) => {
			const active = effect.get(broadcast.active);
			if (!active) return;

			const statusTrack = active.subscribe("status", 10);
			effect.cleanup(() => statusTrack.close());

			effect.spawn(async () => {
				for (;;) {
					const json = (await Promise.race([effect.cancel, statusTrack.readJson()])) as
						| DroneStatus
						| undefined;
					if (!json) break;
					status.set(json);
				}
			});
		});

		// Update controller count from status.
		this.#signals.run((effect) => {
			const st = effect.get(status);
			if (!st) return;

			const statusText = controls.querySelector(".status-text") as HTMLElement | null;
			if (statusText) {
				const n = st.controllers.length;
				statusText.textContent = n === 0 ? "No controllers" : `${n} controller${n > 1 ? "s" : ""}`;
			}
		});

		// Controller alert: yellow for 1, red for 2+.
		this.#signals.run((effect) => {
			const st = effect.get(status);
			const ctrls = st?.controllers ?? [];

			if (ctrls.length === 0) {
				alert.style.display = "none";
			} else if (ctrls.length === 1) {
				alert.style.display = "block";
				alert.className = "alert yellow";
				alert.textContent = `CONTROLLED: ${ctrls[0]}`;
			} else {
				alert.style.display = "block";
				alert.className = "alert red";
				alert.textContent = `${ctrls.length} CONTROLLERS: ${ctrls.join(", ")}`;
			}
		});

		// Command publishing.
		let commandTrack: Moq.Track | undefined;

		this.#signals.run((effect) => {
			const conn = effect.get(connection.established);
			if (!conn) return;

			const exp = effect.get(expanded);
			if (exp !== droneId) return;

			const viewerId = `v${Math.random().toString(36).slice(2, 8)}`;
			const viewerBroadcast = new Moq.Broadcast();
			conn.publish(Moq.Path.from(`drone/${droneId}/viewer/${viewerId}`), viewerBroadcast);
			effect.cleanup(() => {
				viewerBroadcast.close();
				commandTrack = undefined;
			});

			effect.spawn(async () => {
				for (;;) {
					const req = await Promise.race([effect.cancel, viewerBroadcast.requested()]);
					if (!req) break;
					if (req.track.name === "command") commandTrack = req.track;
				}
			});
		});

		this.#sendCommand = (cmd: unknown) => {
			if (!commandTrack) return;
			commandTrack.writeJson(cmd);
		};
	}

	buildControls(): HTMLElement {
		const wrapper = document.createElement("div");
		wrapper.className = "controls-inner";

		// D-pad layout.
		const dpad = document.createElement("div");
		dpad.className = "dpad";

		const makeBtn = (action: string, label: string, className?: string) => {
			const btn = document.createElement("button");
			btn.textContent = label;
			btn.dataset.action = action;
			btn.setAttribute("aria-label", action.charAt(0).toUpperCase() + action.slice(1));
			if (className) btn.className = className;
			btn.addEventListener("click", (e) => {
				e.stopPropagation();
				this.#sendCommand({ type: "action", name: action });
			});
			return btn;
		};

		// Arrow buttons in a 3x3 grid.
		const upBtn = makeBtn("up", "▲", "dpad-btn dpad-up");
		const leftBtn = makeBtn("left", "◄", "dpad-btn dpad-left");
		const rightBtn = makeBtn("right", "►", "dpad-btn dpad-right");
		const downBtn = makeBtn("down", "▼", "dpad-btn dpad-down");

		dpad.appendChild(upBtn);
		dpad.appendChild(leftBtn);
		dpad.appendChild(rightBtn);
		dpad.appendChild(downBtn);

		// Action buttons column.
		const actions = document.createElement("div");
		actions.className = "action-column";

		const grabBtn = makeBtn("grab", "GRAB [Space]", "action-main");
		const dockBtn = makeBtn("dock", "DOCK", "action-dock");

		actions.appendChild(grabBtn);
		actions.appendChild(dockBtn);

		// Status text.
		const statusText = document.createElement("div");
		statusText.className = "status-text";
		statusText.textContent = "Idle";
		actions.appendChild(statusText);

		wrapper.appendChild(dpad);
		wrapper.appendChild(actions);

		return wrapper;
	}

	close() {
		this.#signals.close();
	}
}

function formatTime(s: number): string {
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	return h > 0 ? `${h}h${m}m` : `${m}m${s % 60}s`;
}
