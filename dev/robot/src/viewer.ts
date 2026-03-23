import "@moq/watch/element";
import * as Moq from "@moq/lite";
import type MoqWatch from "@moq/watch/element";

interface SensorData {
	battery: number;
	temp: number;
	gps: [number, number];
	uptime: number;
}

interface StatusData {
	angle: number;
	controllers: string[];
	killed: boolean;
}

/**
 * Robot viewer: video player + sensor HUD + control panel.
 */
export class RobotViewer {
	element: HTMLDivElement;
	#signals = new Moq.Signals.Effect();
	#viewerId: string;
	#commandTrack: Moq.Track | undefined;

	constructor(connection: Moq.Connection.Reload, robotId: string, relayUrl?: string) {
		this.#viewerId = `viewer${Math.random().toString(36).slice(2, 8)}`;

		this.element = document.createElement("div");
		this.element.style.cssText = "max-width: 1200px; margin: 0 auto;";

		// Back button
		const backBtn = document.createElement("button");
		backBtn.textContent = "Back to Fleet";
		backBtn.style.cssText = `
			background: #222; color: #aaa; border: 1px solid #444; padding: 0.5rem 1rem;
			border-radius: 4px; cursor: pointer; margin-bottom: 1rem; font-size: 0.9rem;
		`;
		backBtn.addEventListener("click", () => {
			const url = new URL(window.location.href);
			url.searchParams.delete("robot");
			window.location.href = url.toString();
		});
		this.element.appendChild(backBtn);

		// Title
		const title = document.createElement("h2");
		title.textContent = `Robot: ${robotId}`;
		title.style.cssText = "color: #4ade80; font-family: monospace; margin-bottom: 1rem;";
		this.element.appendChild(title);

		// Video container with HUD overlay
		const videoContainer = document.createElement("div");
		videoContainer.style.cssText = `
			position: relative; background: #000; border-radius: 8px;
			overflow: hidden; margin-bottom: 1rem; aspect-ratio: 16/9;
		`;
		this.element.appendChild(videoContainer);

		// Create <moq-watch> for video playback
		const watchEl = document.createElement("moq-watch") as MoqWatch;
		watchEl.style.cssText = "display: block; width: 100%; height: 100%;";
		const canvas = document.createElement("canvas");
		canvas.style.cssText = "width: 100%; height: 100%; object-fit: contain;";
		watchEl.appendChild(canvas);
		videoContainer.appendChild(watchEl);

		// Point the watch element at the robot broadcast.
		// moq-watch manages its own connection internally.
		watchEl.url = relayUrl ?? connection.url.peek()?.toString();
		watchEl.name = `robot/${robotId}`;

		// Sensor HUD overlay
		const hud = document.createElement("div");
		hud.style.cssText = `
			position: absolute; top: 0.75rem; left: 0.75rem;
			background: rgba(0,0,0,0.7); color: #4ade80; padding: 0.75rem;
			border-radius: 6px; font-family: monospace; font-size: 0.8rem;
			line-height: 1.6; pointer-events: none;
		`;
		hud.textContent = "Waiting for sensor data...";
		videoContainer.appendChild(hud);

		// Status display
		const statusPanel = document.createElement("div");
		statusPanel.style.cssText = `
			background: #1a1a1a; border: 1px solid #333; border-radius: 8px;
			padding: 1rem; margin-bottom: 1rem; font-size: 0.9rem;
		`;
		this.element.appendChild(statusPanel);

		// Controller warning banner
		const warningBanner = document.createElement("div");
		warningBanner.style.cssText = `
			background: #433600; border: 1px solid #facc15; border-radius: 6px;
			padding: 0.75rem 1rem; margin-bottom: 1rem; color: #facc15;
			font-size: 0.85rem; display: none;
		`;
		this.element.appendChild(warningBanner);

		// Control panel
		const controls = document.createElement("div");
		controls.style.cssText = `
			display: flex; gap: 1rem; align-items: center; flex-wrap: wrap;
		`;
		this.element.appendChild(controls);

		// Take Control button
		const takeControlBtn = document.createElement("button");
		takeControlBtn.textContent = "Take Control";
		takeControlBtn.style.cssText = `
			background: #1a4a1a; color: #4ade80; border: 1px solid #4ade80;
			padding: 0.75rem 1.5rem; border-radius: 6px; cursor: pointer;
			font-size: 1rem; font-weight: 600;
		`;
		controls.appendChild(takeControlBtn);

		// Angle buttons (initially hidden)
		const angleContainer = document.createElement("div");
		angleContainer.style.cssText = "display: none; gap: 0.5rem; align-items: center;";
		const angleLabel = document.createElement("span");
		angleLabel.textContent = "Angle:";
		angleLabel.style.cssText = "color: #888; font-size: 0.9rem;";
		angleContainer.appendChild(angleLabel);

		for (let i = 1; i <= 3; i++) {
			const btn = document.createElement("button");
			btn.textContent = String(i);
			btn.style.cssText = `
				background: #222; color: #e0e0e0; border: 1px solid #555;
				padding: 0.5rem 1rem; border-radius: 4px; cursor: pointer;
				font-family: monospace; min-width: 3rem;
			`;
			btn.addEventListener("click", () => this.#sendCommand({ type: "angle", value: i }));
			angleContainer.appendChild(btn);
		}
		controls.appendChild(angleContainer);

		// Kill switch
		const killBtn = document.createElement("button");
		killBtn.textContent = "KILL";
		killBtn.style.cssText = `
			background: #4a1a1a; color: #f87171; border: 2px solid #f87171;
			padding: 0.75rem 1.5rem; border-radius: 6px; cursor: pointer;
			font-size: 1rem; font-weight: 700; display: none; margin-left: auto;
		`;
		killBtn.addEventListener("click", () => this.#sendCommand({ type: "kill" }));
		controls.appendChild(killBtn);

		// Viewer ID display
		const viewerInfo = document.createElement("div");
		viewerInfo.textContent = `Viewer ID: ${this.#viewerId}`;
		viewerInfo.style.cssText = "color: #555; font-size: 0.75rem; font-family: monospace; margin-top: 1rem;";
		this.element.appendChild(viewerInfo);

		// Subscribe to sensor and status tracks.
		this.#signals.run((effect) => {
			const conn = effect.get(connection.established);
			if (!conn) return;

			const broadcast = conn.consume(Moq.Path.from(`robot/${robotId}`));
			effect.cleanup(() => broadcast.close());

			// Sensor track
			const sensorTrack = broadcast.subscribe("sensor", 10);
			effect.cleanup(() => sensorTrack.close());

			effect.spawn(async () => {
				for (;;) {
					const json = await Promise.race([effect.cancel, sensorTrack.readJson()]);
					if (json === undefined) break;
					const data = json as SensorData;
					hud.textContent = [
						`BAT: ${data.battery}%`,
						`TMP: ${data.temp.toFixed(1)}C`,
						`GPS: ${data.gps[0].toFixed(4)}, ${data.gps[1].toFixed(4)}`,
						`UP:  ${formatUptime(data.uptime)}`,
					].join("\n");
					hud.style.whiteSpace = "pre";
				}
			});

			// Status track
			const statusTrack = broadcast.subscribe("status", 10);
			effect.cleanup(() => statusTrack.close());

			effect.spawn(async () => {
				for (;;) {
					const json = await Promise.race([effect.cancel, statusTrack.readJson()]);
					if (json === undefined) break;
					const data = json as StatusData;

					statusPanel.textContent = `Angle: ${data.angle} | Killed: ${data.killed ? "YES" : "No"} | Controllers: ${data.controllers.length > 0 ? data.controllers.join(", ") : "None"}`;

					// Show warning if other controllers.
					const otherControllers = data.controllers.filter((c) => c !== this.#viewerId);
					if (otherControllers.length > 0) {
						warningBanner.textContent = `Other controllers active: ${otherControllers.join(", ")}`;
						warningBanner.style.display = "block";
					} else {
						warningBanner.style.display = "none";
					}
				}
			});
		});

		// Take Control handler: publish viewer broadcast with command track.
		takeControlBtn.addEventListener("click", () => {
			takeControlBtn.style.display = "none";
			angleContainer.style.display = "flex";
			killBtn.style.display = "block";

			this.#signals.run((effect) => {
				const conn = effect.get(connection.established);
				if (!conn) return;

				const viewerPath = Moq.Path.from(`robot/${robotId}/viewer/${this.#viewerId}`);
				const broadcast = new Moq.Broadcast();
				conn.publish(viewerPath, broadcast);
				effect.cleanup(() => broadcast.close());

				effect.spawn(async () => {
					for (;;) {
						const req = await Promise.race([effect.cancel, broadcast.requested()]);
						if (!req) break;

						if (req.track.name === "command") {
							this.#commandTrack = req.track;
						}
					}
				});
			});
		});
	}

	#sendCommand(cmd: unknown) {
		if (!this.#commandTrack) {
			console.warn("No command track available yet");
			return;
		}
		this.#commandTrack.writeJson(cmd);
	}

	close() {
		this.#signals.close();
	}
}

function formatUptime(seconds: number): string {
	const h = Math.floor(seconds / 3600);
	const m = Math.floor((seconds % 3600) / 60);
	const s = seconds % 60;
	return `${h}h ${m}m ${s}s`;
}
