import * as Moq from "@moq/lite";
import * as Watch from "@moq/watch";

// Parse URL params.
const params = new URLSearchParams(window.location.search);
const url = new URL(params.get("url") ?? import.meta.env.VITE_RELAY_URL ?? "https://cdn.moq.dev/anon");

const statusEl = document.getElementById("connection-status")!;
const gridEl = document.getElementById("grid")!;

// Single shared connection for everything.
const connection = new Moq.Connection.Reload({ url, enabled: true });

// Track connection status.
const root = new Moq.Signals.Effect();
root.run((e) => {
	const status = e.get(connection.status);
	statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
	statusEl.style.color = status === "connected" ? "#4ade80" : status === "connecting" ? "#facc15" : "#888";
});

// Track which robo is expanded (fullscreen).
const expanded = new Moq.Signals.Signal<string | undefined>(undefined);

// Active robo cards.
const robos = new Map<string, RoboCard>();

// Discover robos via announcements.
root.run((effect) => {
	const conn = effect.get(connection.established);
	if (!conn) return;

	const announced = conn.announced(Moq.Path.from("robo"));
	effect.cleanup(() => announced.close());

	effect.spawn(async () => {
		for (;;) {
			const entry = await Promise.race([effect.cancel, announced.next()]);
			if (!entry) break;

			// Strip the "robo/" prefix to get the robo ID.
			// Skip nested paths like "robo/abc123/viewer/..."
			const suffix = Moq.Path.stripPrefix(Moq.Path.from("robo"), entry.path);
			if (!suffix || suffix.includes("/")) continue;

			const id = suffix;
			if (entry.active && !robos.has(id)) {
				const card = new RoboCard(id);
				robos.set(id, card);
				gridEl.appendChild(card.el);
			} else if (!entry.active) {
				const card = robos.get(id);
				if (card) {
					card.close();
					card.el.remove();
					robos.delete(id);
				}
			}
		}
	});
});

interface RoboStatus {
	actions: string[];
	current: string;
	queued: string | null;
	controllers: string[];
}

// A robo card: live video + sensor HUD, expandable to fullscreen with controls.
class RoboCard {
	el: HTMLDivElement;
	#signals = new Moq.Signals.Effect();

	constructor(roboId: string) {
		this.el = document.createElement("div");
		this.el.className = "card";

		// Canvas for video.
		const canvas = document.createElement("canvas");
		canvas.className = "video";
		this.el.appendChild(canvas);

		// Label overlay.
		const label = document.createElement("div");
		label.className = "label";
		label.textContent = roboId;
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

		// Controls (visible when expanded).
		const controls = document.createElement("div");
		controls.className = "controls";
		this.el.appendChild(controls);

		// Click to toggle expand.
		this.el.addEventListener("click", () => {
			expanded.set(expanded.peek() === roboId ? undefined : roboId);
		});

		// React to expand state for styling.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const isExpanded = exp === roboId;
			this.el.classList.toggle("expanded", isExpanded);
			controls.style.display = isExpanded ? "flex" : "none";
		});

		// Set up video via Watch API, sharing the connection.
		const broadcast = new Watch.Broadcast({
			connection: connection.established,
			name: Moq.Path.from(`robo/${roboId}`),
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
			const pixels = exp === roboId ? 1920 * 1080 : 426 * 240;
			videoSource.target.set({ pixels });
		});

		const videoDecoder = new Watch.Video.Decoder(videoSource);
		this.#signals.cleanup(() => videoDecoder.close());

		// Disable non-expanded cards when one is expanded to save bandwidth.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const active = exp === undefined || exp === roboId;
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
		const status = new Moq.Signals.Signal<RoboStatus | undefined>(undefined);

		this.#signals.run((effect) => {
			const active = effect.get(broadcast.active);
			if (!active) return;

			const statusTrack = active.subscribe("status", 10);
			effect.cleanup(() => statusTrack.close());

			effect.spawn(async () => {
				for (;;) {
					const json = (await Promise.race([effect.cancel, statusTrack.readJson()])) as
						| RoboStatus
						| undefined;
					if (!json) break;
					status.set(json);
				}
			});
		});

		// Dynamic action buttons + kill button based on status.
		this.#signals.run((effect) => {
			const st = effect.get(status);
			if (!st) return;

			// Clear existing buttons.
			while (controls.firstChild) {
				controls.removeChild(controls.firstChild);
			}

			// Create action buttons.
			for (const action of st.actions) {
				const btn = document.createElement("button");
				btn.textContent = capitalize(action);
				btn.className = "action-btn";

				if (st.current === action) {
					btn.classList.add("active");
				} else if (st.queued === action) {
					btn.classList.add("queued");
				}

				btn.addEventListener("click", (e) => {
					e.stopPropagation();
					sendCommand({ type: "action", name: action });
				});
				controls.appendChild(btn);
			}

			// Kill button.
			const killBtn = document.createElement("button");
			killBtn.textContent = "KILL";
			killBtn.className = "kill";
			if (st.current === "dead") {
				killBtn.classList.add("active");
			}
			killBtn.addEventListener("click", (e) => {
				e.stopPropagation();
				sendCommand({ type: "kill" });
			});
			controls.appendChild(killBtn);
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
			if (exp !== roboId) return;

			const viewerId = `v${Math.random().toString(36).slice(2, 8)}`;
			const viewerBroadcast = new Moq.Broadcast();
			conn.publish(Moq.Path.from(`robo/${roboId}/viewer/${viewerId}`), viewerBroadcast);
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

		function sendCommand(cmd: unknown) {
			if (!commandTrack) return;
			commandTrack.writeJson(cmd);
		}
	}

	close() {
		this.#signals.close();
	}
}

function capitalize(s: string): string {
	return s.charAt(0).toUpperCase() + s.slice(1);
}

function formatTime(s: number): string {
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	return h > 0 ? `${h}h${m}m` : `${m}m${s % 60}s`;
}
