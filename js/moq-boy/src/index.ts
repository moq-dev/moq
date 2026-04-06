import * as Moq from "@moq/lite";
import * as Watch from "@moq/watch";

export { Moq, Watch };

export interface GameStats {
	video_secs: number;
	audio_secs: number;
	emulation_secs: number;
	wall_secs: number;
}

export interface GameStatus {
	buttons: string[];
	reset_in: number;
	latency: Record<string, number>;
	stats?: GameStats;
}

export interface GameCardConfig {
	sessionId: string;
	connection: Moq.Connection.Reload;
	expanded: Moq.Signals.Signal<string | undefined>;
	root: ShadowRoot | HTMLElement;
}

// Key mapping for keyboard input.
const KEY_MAP: Record<string, string> = {
	ArrowUp: "up",
	ArrowDown: "down",
	ArrowLeft: "left",
	ArrowRight: "right",
	z: "b",
	Z: "b",
	x: "a",
	X: "a",
	Enter: "start",
	Shift: "select",
};

// A game session card: live video + audio, expandable with controls.
export class GameCard {
	el: HTMLDivElement;
	#signals = new Moq.Signals.Effect();
	#sendCommand: (cmd: Record<string, unknown>) => void = () => {};
	#heldButtons = new Set<string>();

	constructor(config: GameCardConfig) {
		const { sessionId, connection, expanded } = config;

		this.el = document.createElement("div");
		this.el.className = "card";

		// Canvas for video.
		const canvas = document.createElement("canvas");
		canvas.className = "video";
		this.el.appendChild(canvas);

		// Label overlay.
		const label = document.createElement("div");
		label.className = "label";
		label.textContent = sessionId;
		this.el.appendChild(label);

		// Countdown overlay.
		const countdown = document.createElement("div");
		countdown.className = "countdown";
		this.el.appendChild(countdown);

		// Controls container.
		const controls = document.createElement("div");
		controls.className = "controls";
		this.el.appendChild(controls);

		// Build controls.
		const { wrapper: controlsInner, latencyList, statsList, muteBtn } = this.#buildControls();
		controls.appendChild(controlsInner);

		// Click to toggle expand via Fullscreen API.
		canvas.addEventListener("click", () => {
			if (expanded.peek() === sessionId) {
				document.exitFullscreen().catch(() => {});
			} else {
				expanded.set(sessionId);
				this.el.requestFullscreen().catch(() => {});
			}
		});

		// Listen for fullscreen exit to sync expanded state.
		const onFullscreenChange = () => {
			if (!document.fullscreenElement && expanded.peek() === sessionId) {
				expanded.set(undefined);
			}
		};
		document.addEventListener("fullscreenchange", onFullscreenChange);
		this.#signals.cleanup(() => document.removeEventListener("fullscreenchange", onFullscreenChange));

		// React to expand state.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const isExpanded = exp === sessionId;
			this.el.classList.toggle("expanded", isExpanded);
			controls.style.display = isExpanded ? "flex" : "none";

			if (!isExpanded && this.#heldButtons.size > 0) {
				this.#heldButtons.clear();
				this.#sendButtons();
			}
		});

		// Keyboard input when expanded.
		const onKeyDown = (e: KeyboardEvent) => {
			if (expanded.peek() !== sessionId) return;
			if (e.repeat) return;

			const button = KEY_MAP[e.key];
			if (button) {
				this.#heldButtons.add(button);
				this.#sendButtons();
				e.preventDefault();
			} else if (e.key === "Escape") {
				document.exitFullscreen().catch(() => {});
				e.preventDefault();
			}
		};
		const onKeyUp = (e: KeyboardEvent) => {
			if (expanded.peek() !== sessionId) return;
			const button = KEY_MAP[e.key];
			if (button) {
				this.#heldButtons.delete(button);
				this.#sendButtons();
				e.preventDefault();
			}
		};
		const onBlur = () => {
			if (this.#heldButtons.size > 0) {
				this.#heldButtons.clear();
				this.#sendButtons();
			}
		};
		document.addEventListener("keydown", onKeyDown);
		document.addEventListener("keyup", onKeyUp);
		window.addEventListener("blur", onBlur);
		this.#signals.cleanup(() => {
			document.removeEventListener("keydown", onKeyDown);
			document.removeEventListener("keyup", onKeyUp);
			window.removeEventListener("blur", onBlur);
			if (this.#heldButtons.size > 0) {
				this.#heldButtons.clear();
				this.#sendButtons();
			}
		});

		// Set up video via Watch API.
		const broadcast = new Watch.Broadcast({
			connection: connection.established,
			name: Moq.Path.from(`boy/${sessionId}`),
			enabled: true,
		});
		this.#signals.cleanup(() => broadcast.close());

		const sync = new Watch.Sync({ jitter: 50 as Moq.Time.Milli });
		this.#signals.cleanup(() => sync.close());

		const videoSource = new Watch.Video.Source(sync, { broadcast });
		this.#signals.cleanup(() => videoSource.close());

		// Set pixel budget based on expanded state.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			// Native GB is 160x144 = 23040 pixels. When expanded, allow more for quality.
			const pixels = exp === sessionId ? 160 * 144 * 4 : 160 * 144;
			videoSource.target.set({ pixels });
		});

		const videoDecoder = new Watch.Video.Decoder(videoSource);
		this.#signals.cleanup(() => videoDecoder.close());

		// Disable non-expanded cards to save bandwidth.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const active = exp === undefined || exp === sessionId;
			videoDecoder.enabled.set(active);
		});

		const videoRenderer = new Watch.Video.Renderer(videoDecoder, { canvas });
		this.#signals.cleanup(() => videoRenderer.close());

		// Set up audio — play on hover at 50% volume.
		const audioSource = new Watch.Audio.Source(sync, { broadcast });
		this.#signals.cleanup(() => audioSource.close());

		const audioDecoder = new Watch.Audio.Decoder(audioSource);
		this.#signals.cleanup(() => audioDecoder.close());

		const audioEmitter = new Watch.Audio.Emitter(audioDecoder, { volume: 0.5, muted: true });
		this.#signals.cleanup(() => audioEmitter.close());

		// Resume AudioContext on first user interaction (browser autoplay policy).
		const resumeEvents = ["click", "touchstart", "touchend", "mousedown", "keydown"];
		const resumeAudio = () => {
			const ctx = audioDecoder.context.peek();
			if (ctx && ctx.state === "suspended") {
				ctx.resume();
			}
		};
		for (const event of resumeEvents) {
			document.addEventListener(event, resumeAudio, { once: true });
		}
		this.#signals.cleanup(() => {
			for (const event of resumeEvents) {
				document.removeEventListener(event, resumeAudio);
			}
		});

		// Track hover state.
		const hovered = new Moq.Signals.Signal(false);
		this.el.addEventListener("mouseenter", () => hovered.set(true));
		this.el.addEventListener("mouseleave", () => hovered.set(false));

		// Enable audio decoding when hovered or expanded.
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const hover = effect.get(hovered);
			audioDecoder.enabled.set(exp === sessionId || hover);
		});

		// Unmute on hover/expand (user gesture satisfies autoplay policy).
		const userMuted = new Moq.Signals.Signal(false);
		this.#signals.run((effect) => {
			const exp = effect.get(expanded);
			const hover = effect.get(hovered);
			const muted = effect.get(userMuted);
			audioEmitter.muted.set(muted || !(exp === sessionId || hover));
		});

		// Subscribe to status track for button highlights and countdown.
		this.#signals.run((effect) => {
			const active = effect.get(broadcast.active);
			if (!active) return;

			const statusTrack = active.subscribe("status", 10);
			effect.cleanup(() => statusTrack.close());

			effect.spawn(async () => {
				for (;;) {
					const json = (await Promise.race([effect.cancel, statusTrack.readJson()])) as
						| GameStatus
						| undefined;
					if (!json) break;

					// Highlight currently pressed buttons.
					const allBtns = controls.querySelectorAll("[data-button]");
					for (const btn of allBtns) {
						const name = (btn as HTMLElement).dataset.button;
						(btn as HTMLElement).classList.toggle("active", json.buttons.includes(name ?? ""));
					}

					// Show countdown when approaching timeout.
					if (json.reset_in <= 60) {
						countdown.style.display = "block";
						countdown.textContent = `Reset in ${json.reset_in}s`;
					} else {
						countdown.style.display = "none";
					}

					// Show per-viewer latency.
					const vid = currentViewerId.peek();
					const entries = Object.entries(json.latency ?? {});

					// Update label with viewer count.
					const n = entries.length;
					label.textContent = n > 0 ? `${sessionId} (${n})` : sessionId;

					// Controls panel: show all viewers with latency.
					latencyList.replaceChildren();
					if (entries.length > 0) {
						const header = document.createElement("div");
						header.className = "latency-header";
						header.textContent = `Latency (${n} viewer${n !== 1 ? "s" : ""})`;
						latencyList.appendChild(header);
						for (const [id, ms] of entries) {
							const row = document.createElement("div");
							row.className = id === vid ? "latency-entry self" : "latency-entry";
							const nameSpan = document.createElement("span");
							nameSpan.textContent = id === vid ? `${id} (you)` : id;
							const msSpan = document.createElement("span");
							msSpan.textContent = `${ms}ms`;
							row.appendChild(nameSpan);
							row.appendChild(msSpan);
							latencyList.appendChild(row);
						}
					}

					// Stats panel: show encoding/emulation time.
					if (json.stats) {
						const s = json.stats;
						const pct = (v: number) => (s.wall_secs > 0 ? Math.round((v / s.wall_secs) * 100) : 0);

						statsList.replaceChildren();
						const statsHeader = document.createElement("div");
						statsHeader.className = "stats-header";
						statsHeader.textContent = `Stats (${s.wall_secs}s)`;
						statsList.appendChild(statsHeader);

						const items: [string, number][] = [
							["Video", s.video_secs],
							["Audio", s.audio_secs],
							["Emulation", s.emulation_secs],
						];

						for (const [itemLabel, secs] of items) {
							const row = document.createElement("div");
							row.className = "stats-entry";
							const nameSpan = document.createElement("span");
							nameSpan.textContent = itemLabel;
							const valSpan = document.createElement("span");
							valSpan.textContent = `${secs}s (${pct(secs)}%)`;
							row.appendChild(nameSpan);
							row.appendChild(valSpan);
							statsList.appendChild(row);
						}
					}
				}
			});
		});

		// Command publishing.
		let commandTrack: Moq.Track | undefined;
		const currentViewerId = new Moq.Signals.Signal<string | undefined>(undefined);

		this.#signals.run((effect) => {
			const conn = effect.get(connection.established);
			if (!conn) return;

			const exp = effect.get(expanded);
			if (exp !== sessionId) return;

			const viewerId = Math.random().toString(36).slice(2, 8);
			currentViewerId.set(viewerId);
			const viewerBroadcast = new Moq.Broadcast();
			conn.publish(Moq.Path.from(`boy/${sessionId}/viewer/${viewerId}`), viewerBroadcast);
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

		this.#sendCommand = (cmd: Record<string, unknown>) => {
			if (!commandTrack) return;
			// Attach the current video timestamp so the publisher can measure latency.
			const ts = videoDecoder.timestamp.peek();
			commandTrack.writeJson({ ...cmd, ts: ts ?? 0 });
		};

		// Wire up mute toggle button.
		muteBtn.textContent = "Mute";
		muteBtn.addEventListener("click", (e) => {
			e.stopPropagation();
			const muted = !userMuted.peek();
			userMuted.set(muted);
			muteBtn.textContent = muted ? "Unmute" : "Mute";
			muteBtn.classList.toggle("unmuted", !muted);
		});
	}

	#buildControls(): {
		wrapper: HTMLElement;
		latencyList: HTMLElement;
		statsList: HTMLElement;
		muteBtn: HTMLButtonElement;
	} {
		const wrapper = document.createElement("div");
		wrapper.className = "controls-inner";

		// D-pad
		const dpad = document.createElement("div");
		dpad.className = "dpad";

		const makeBtn = (className: string, label: string, buttonName: string) => {
			const btn = document.createElement("button");
			btn.textContent = label;
			btn.className = className;
			btn.dataset.button = buttonName;
			btn.addEventListener("mousedown", (e) => {
				e.stopPropagation();
				this.#heldButtons.add(buttonName);
				this.#sendButtons();
			});
			btn.addEventListener("mouseup", (e) => {
				e.stopPropagation();
				this.#heldButtons.delete(buttonName);
				this.#sendButtons();
			});
			btn.addEventListener("mouseleave", () => {
				if (this.#heldButtons.has(buttonName)) {
					this.#heldButtons.delete(buttonName);
					this.#sendButtons();
				}
			});
			btn.addEventListener("touchstart", (e) => {
				e.preventDefault();
				this.#heldButtons.add(buttonName);
				this.#sendButtons();
			});
			btn.addEventListener("touchend", (e) => {
				e.preventDefault();
				this.#heldButtons.delete(buttonName);
				this.#sendButtons();
			});
			return btn;
		};

		dpad.appendChild(makeBtn("dpad-btn dpad-up", "\u25B2", "up"));
		dpad.appendChild(makeBtn("dpad-btn dpad-left", "\u25C4", "left"));
		dpad.appendChild(makeBtn("dpad-btn dpad-right", "\u25BA", "right"));
		dpad.appendChild(makeBtn("dpad-btn dpad-down", "\u25BC", "down"));

		// A/B buttons
		const abBtns = document.createElement("div");
		abBtns.className = "ab-buttons";
		abBtns.appendChild(makeBtn("ab-btn", "B", "b"));
		abBtns.appendChild(makeBtn("ab-btn", "A", "a"));

		// Start/Select
		const metaBtns = document.createElement("div");
		metaBtns.className = "meta-buttons";
		metaBtns.appendChild(makeBtn("meta-btn", "Select", "select"));
		metaBtns.appendChild(makeBtn("meta-btn", "Start", "start"));

		// Utility buttons
		const utilBtns = document.createElement("div");
		utilBtns.className = "util-buttons";

		const muteBtn = document.createElement("button");
		muteBtn.className = "util-btn mute-btn";
		muteBtn.textContent = "Unmute";
		utilBtns.appendChild(muteBtn);

		const resetBtn = document.createElement("button");
		resetBtn.className = "util-btn reset";
		resetBtn.textContent = "Reset";
		resetBtn.addEventListener("click", (e) => {
			e.stopPropagation();
			this.#sendCommand({ type: "reset" });
		});
		utilBtns.appendChild(resetBtn);

		// Key hints
		const hints = document.createElement("div");
		hints.className = "key-hints";
		const lines = ["Arrows: D-pad", "Z: B \u00A0 X: A", "Enter: Start \u00A0 Shift: Select", "Esc: Exit"];
		for (const line of lines) {
			const div = document.createElement("div");
			div.textContent = line;
			hints.appendChild(div);
		}

		// Latency list (populated by status track)
		const latencyList = document.createElement("div");
		latencyList.className = "latency-list";

		wrapper.appendChild(dpad);
		wrapper.appendChild(abBtns);
		wrapper.appendChild(metaBtns);
		wrapper.appendChild(utilBtns);

		const latencyNote = document.createElement("div");
		latencyNote.className = "latency-note";
		latencyNote.textContent = "Includes both the render delay AND the input delay.";

		// Stats list (populated by status track)
		const statsList = document.createElement("div");
		statsList.className = "stats-list";

		wrapper.appendChild(hints);
		wrapper.appendChild(latencyList);
		wrapper.appendChild(statsList);
		wrapper.appendChild(latencyNote);

		return { wrapper, latencyList, statsList, muteBtn };
	}

	#sendButtons() {
		this.#sendCommand({ type: "buttons", buttons: [...this.#heldButtons] });
	}

	close() {
		this.#signals.close();
	}
}
