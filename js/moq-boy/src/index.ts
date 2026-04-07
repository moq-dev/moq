import * as Moq from "@moq/lite";
import * as Watch from "@moq/watch";

export type { default as MoqBoy } from "./element.ts";
export type { GameStats, GameStatus } from "./schemas.ts";
export { GameStatsSchema, GameStatusSchema } from "./schemas.ts";
export { Moq, Watch };

import type { GameStatus } from "./schemas.ts";
import { GameStatusSchema } from "./schemas.ts";

/** Configuration for creating a Game instance. */
export interface GameConfig {
	/** Unique session identifier (e.g. the ROM name). */
	sessionId: string;
	/** MoQ connection to the relay. */
	connection: Moq.Connection.Reload;
	/** Shared signal tracking which game is currently expanded. */
	expanded: Moq.Signals.Signal<string | undefined>;
	/** MoQ path prefix for game broadcasts (e.g. "anon/boy/game"). */
	gamePrefix: string;
	/** MoQ path prefix for viewer broadcasts (e.g. "anon/boy/viewer"). */
	viewerPrefix: string;
}

// Stop publishing feedback after 60s of no input.
const FEEDBACK_IDLE_MS = 60_000;

// Game Boy native resolution.
const GB_WIDTH = 160;
const GB_HEIGHT = 144;
const GB_PIXELS = GB_WIDTH * GB_HEIGHT;

// Default jitter buffer in milliseconds.
const DEFAULT_JITTER = 50 as Moq.Time.Milli;

/** Key mapping from keyboard keys to Game Boy buttons. */
export const KEY_MAP: Record<string, string> = {
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

/**
 * A Game Boy streaming session — the non-UI backend.
 *
 * Manages video/audio playback, input commands, and status tracking
 * for a single game session. The UI layer (SolidJS components) reads
 * signals from this class to render the interface.
 */
export class Game {
	readonly sessionId: string;
	readonly #signals = new Moq.Signals.Effect();

	// Config references.
	readonly expanded: Moq.Signals.Signal<string | undefined>;
	readonly #viewerPrefix: string;

	// Reactive state exposed to UI.
	readonly hovered = new Moq.Signals.Signal(false);
	readonly active = new Moq.Signals.Signal(false);
	readonly jitter = new Moq.Signals.Signal<Moq.Time.Milli>(DEFAULT_JITTER);
	readonly userMuted = new Moq.Signals.Signal(false);
	readonly status = new Moq.Signals.Signal<GameStatus | undefined>(undefined);
	readonly viewerId = new Moq.Signals.Signal<string | undefined>(undefined);

	// Watch API objects — exposed so UI can access canvas, etc.
	readonly broadcast: Watch.Broadcast;
	readonly sync: Watch.Sync;
	readonly videoSource: Watch.Video.Source;
	readonly videoDecoder: Watch.Video.Decoder;
	readonly videoRenderer: Watch.Video.Renderer;
	readonly audioSource: Watch.Audio.Source;
	readonly audioDecoder: Watch.Audio.Decoder;
	readonly audioEmitter: Watch.Audio.Emitter;

	// Input state.
	readonly heldButtons = new Set<string>();

	// Internal command publishing state.
	#commandTrack: Moq.Track | undefined;
	#pendingCommand: Record<string, unknown> | undefined;
	#feedbackActive = new Moq.Signals.Signal(false);
	#feedbackTimeout: ReturnType<typeof setTimeout> | undefined;

	constructor(config: GameConfig) {
		const { sessionId, connection, expanded, gamePrefix, viewerPrefix } = config;
		this.sessionId = sessionId;
		this.expanded = expanded;
		this.#viewerPrefix = viewerPrefix;

		// Derive active state from expanded + hovered.
		this.#signals.run(this.#runActive.bind(this));

		// Video pipeline.
		this.broadcast = new Watch.Broadcast({
			connection: connection.established,
			name: Moq.Path.from(`${gamePrefix}/${sessionId}`),
			enabled: true,
		});
		this.#signals.cleanup(() => this.broadcast.close());

		this.sync = new Watch.Sync({ jitter: this.jitter });
		this.#signals.cleanup(() => this.sync.close());

		this.videoSource = new Watch.Video.Source(this.sync, { broadcast: this.broadcast });
		this.#signals.cleanup(() => this.videoSource.close());

		this.#signals.run(this.#runPixelBudget.bind(this));

		// Video is enabled on the grid or when this game is expanded.
		const videoEnabled = new Moq.Signals.Signal(true);
		this.#signals.run(this.#runVideoEnabled.bind(this, videoEnabled));

		this.videoDecoder = new Watch.Video.Decoder(this.videoSource, { enabled: videoEnabled });
		this.#signals.cleanup(() => this.videoDecoder.close());

		// Renderer needs a canvas — created by the UI layer, set via setCanvas().
		this.videoRenderer = new Watch.Video.Renderer(this.videoDecoder);
		this.#signals.cleanup(() => this.videoRenderer.close());

		// Audio pipeline — only download audio when active AND unmuted.
		this.audioSource = new Watch.Audio.Source(this.sync, { broadcast: this.broadcast });
		this.#signals.cleanup(() => this.audioSource.close());

		const audioEnabled = new Moq.Signals.Signal(false);
		this.#signals.run(this.#runAudioEnabled.bind(this, audioEnabled));

		this.audioDecoder = new Watch.Audio.Decoder(this.audioSource, { enabled: audioEnabled });
		this.#signals.cleanup(() => this.audioDecoder.close());

		this.audioEmitter = new Watch.Audio.Emitter(this.audioDecoder, { volume: 0.5 });
		this.#signals.cleanup(() => this.audioEmitter.close());

		// Resume AudioContext on first user interaction (browser autoplay policy).
		for (const event of ["click", "touchstart", "touchend", "mousedown", "keydown"]) {
			this.#signals.event(document, event, () => {
				const ctx = this.audioDecoder.context.peek();
				if (ctx?.state === "suspended") ctx.resume();
			});
		}

		// Subscribe to status track.
		this.#signals.run(this.#runStatus.bind(this));

		// Command publishing.
		this.#signals.run(this.#runCommands.bind(this, connection));
	}

	/** Toggle this game's expanded state. */
	toggleExpand() {
		this.expanded.update((old) => (old === this.sessionId ? undefined : this.sessionId));
	}

	/** Send a button state update. */
	sendButtons() {
		this.sendCommand({ type: "buttons", buttons: [...this.heldButtons] });
	}

	/** Send a command to the emulator. */
	sendCommand(cmd: Record<string, unknown>) {
		// Activate feedback broadcasting on input, with idle timeout.
		this.#feedbackActive.set(true);
		clearTimeout(this.#feedbackTimeout);
		this.#feedbackTimeout = setTimeout(() => this.#feedbackActive.set(false), FEEDBACK_IDLE_MS);

		if (!this.#commandTrack) {
			this.#pendingCommand = cmd;
			return;
		}

		const ts = this.videoDecoder.timestamp.peek();
		this.#commandTrack.writeJson({ ...cmd, ts: ts ?? 0 });
	}

	close() {
		clearTimeout(this.#feedbackTimeout);
		this.#signals.close();
	}

	// --- Effect callbacks ---

	#runActive(effect: Moq.Signals.Effect) {
		const exp = effect.get(this.expanded);
		const hover = effect.get(this.hovered);
		this.active.set(exp === this.sessionId || hover);
	}

	#runPixelBudget(effect: Moq.Signals.Effect) {
		const exp = effect.get(this.expanded);
		// Native GB is 160x144 = 23040 pixels. When expanded, allow 4x for quality.
		const pixels = exp === this.sessionId ? GB_PIXELS * 4 : GB_PIXELS;
		this.videoSource.target.set({ pixels });
	}

	#runVideoEnabled(videoEnabled: Moq.Signals.Signal<boolean>, effect: Moq.Signals.Effect) {
		const exp = effect.get(this.expanded);
		videoEnabled.set(exp === undefined || exp === this.sessionId);
	}

	#runAudioEnabled(audioEnabled: Moq.Signals.Signal<boolean>, effect: Moq.Signals.Effect) {
		const active = effect.get(this.active);
		const userMuted = effect.get(this.userMuted);
		audioEnabled.set(active && !userMuted);
	}

	#runStatus(effect: Moq.Signals.Effect) {
		const active = effect.get(this.broadcast.active);
		if (!active) return;

		const statusTrack = active.subscribe("status", 10);
		effect.cleanup(() => statusTrack.close());

		effect.spawn(async () => {
			for (;;) {
				const json = await Promise.race([effect.cancel, statusTrack.readJson()]);
				if (!json) break;

				const result = GameStatusSchema.safeParse(json);
				if (!result.success) {
					console.warn("Invalid status JSON:", result.error);
					continue;
				}

				this.status.set(result.data);
			}
		});
	}

	#runCommands(connection: Moq.Connection.Reload, effect: Moq.Signals.Effect) {
		const conn = effect.get(connection.established);
		if (!conn) return;

		if (!effect.get(this.active)) {
			// Clear feedback state when deactivating.
			this.#feedbackActive.set(false);
			clearTimeout(this.#feedbackTimeout);
			this.#pendingCommand = undefined;
			return;
		}

		const active = effect.get(this.#feedbackActive);
		if (!active) return;

		const viewerId = Math.random().toString(36).slice(2, 8);
		this.viewerId.set(viewerId);

		const viewerBroadcast = new Moq.Broadcast();
		conn.publish(Moq.Path.from(`${this.#viewerPrefix}/${this.sessionId}/${viewerId}`), viewerBroadcast);
		effect.cleanup(() => {
			viewerBroadcast.close();
			this.#commandTrack = undefined;
			this.viewerId.set(undefined);
		});

		effect.spawn(async () => {
			for (;;) {
				const req = await Promise.race([effect.cancel, viewerBroadcast.requested()]);
				if (!req) break;
				if (req.track.name === "command") {
					this.#commandTrack = req.track;
					// Flush any pending command that triggered activation.
					if (this.#pendingCommand) {
						const ts = this.videoDecoder.timestamp.peek();
						this.#commandTrack.writeJson({ ...this.#pendingCommand, ts: ts ?? 0 });
						this.#pendingCommand = undefined;
					}
				}
			}
		});
	}
}
