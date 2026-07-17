/**
 * The non-UI Game Boy session backend.
 *
 * Lives in its own module (not the package entry) so UI components can import
 * `Game`/`KEY_MAP` without pulling in `index.ts`, which re-exports the element.
 *
 * @module
 */
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";
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

/** A command with captured timestamps, published via the command signal. */
interface Command {
	cmd: Record<string, unknown>;
	timestamps: { label: string; ts: number }[];
}

// Stop publishing feedback after 60s of no input.
const FEEDBACK_IDLE_MS = 60_000;

// Game Boy native resolution.
const GB_WIDTH = 160;
const GB_HEIGHT = 144;
const GB_PIXELS = GB_WIDTH * GB_HEIGHT;

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
 * A Game Boy streaming session for the non-UI backend.
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
	readonly latency = new Moq.Signals.Signal<Watch.Latency>("real-time");
	readonly userMuted = new Moq.Signals.Signal(false);
	readonly volume = new Moq.Signals.Signal(0.25);
	readonly status = new Moq.Signals.Signal<GameStatus | undefined>(undefined);
	readonly viewerId = new Moq.Signals.Signal<string | undefined>(undefined);

	// The canvas to render into, owned here and wired into the renderer (read-only there).
	// The UI sets this once the <canvas> is mounted.
	readonly canvas = new Moq.Signals.Signal<HTMLCanvasElement | undefined>(undefined);

	// The video rendition target, owned here and wired into the video source as an input.
	readonly #target = new Moq.Signals.Signal<Watch.Video.Target | undefined>(undefined);

	// Watch API objects exposed so UI can access canvas, etc.
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
	#command = new Moq.Signals.Signal<Command | undefined>(undefined);
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

		// Sources produce the per-rendition jitter that Sync reads, so they're created
		// before Sync to avoid a construction cycle.
		this.videoSource = new Watch.Video.Source({
			broadcast: this.broadcast,
			target: this.#target,
			supported: Watch.Video.Decoder.supported,
		});
		this.#signals.cleanup(() => this.videoSource.close());

		this.audioSource = new Watch.Audio.Source({
			broadcast: this.broadcast,
			supported: Watch.Audio.Decoder.supported,
		});
		this.#signals.cleanup(() => this.audioSource.close());

		this.sync = new Watch.Sync({
			latency: this.latency,
			connection: connection.established,
			video: this.videoSource.out.jitter,
			audio: this.audioSource.out.jitter,
		});
		this.#signals.cleanup(() => this.sync.close());

		this.#signals.run(this.#runPixelBudget.bind(this));

		const videoEnabled = new Moq.Signals.Signal(true);
		this.videoDecoder = new Watch.Video.Decoder(this.videoSource, this.sync, { enabled: videoEnabled });
		this.#signals.cleanup(() => this.videoDecoder.close());

		// Renderer needs a canvas created by the UI layer, set via `canvas`.
		this.videoRenderer = new Watch.Video.Renderer(this.videoDecoder, { canvas: this.canvas });
		this.#signals.cleanup(() => this.videoRenderer.close());

		// Download on the grid or when expanded, but only while the tile is on-screen.
		// Reads the renderer's visibility, so it must run after the renderer exists.
		this.#signals.run(this.#runVideoEnabled.bind(this, videoEnabled));

		// Audio pipeline. The emitter stops the download when muted or paused.
		this.audioDecoder = new Watch.Audio.Decoder(this.audioSource, this.sync);
		this.#signals.cleanup(() => this.audioDecoder.close());

		const audioPaused = new Moq.Signals.Signal(true);
		this.#signals.run(this.#runAudioPaused.bind(this, audioPaused));

		this.audioEmitter = new Watch.Audio.Emitter(this.audioDecoder, {
			volume: this.volume,
			muted: this.userMuted,
			paused: audioPaused,
		});
		this.#signals.cleanup(() => this.audioEmitter.close());

		// Resume AudioContext on first user interaction (browser autoplay policy).
		for (const event of ["click", "touchstart", "touchend", "mousedown", "keydown"]) {
			this.#signals.event(document, event, () => {
				const ctx = this.audioDecoder.out.context.peek();
				if (ctx?.state === "suspended") ctx.resume();
			});
		}

		// Subscribe to status track.
		this.#signals.run(this.#runStatus.bind(this));

		// Command publishing.
		this.#signals.run(this.#runCommands.bind(this, connection));
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

		this.#command.set({ cmd, timestamps: this.#timestamps() }, true);
	}

	/** Collect media timestamps at each pipeline stage for latency measurement. */
	#timestamps(): { label: string; ts: number }[] {
		const entries: { label: string; ts: number }[] = [];
		const received = this.sync.out.timestamp.peek();
		if (received != null) entries.push({ label: "received", ts: received });
		const decoded = this.videoDecoder.out.timestamp.peek();
		if (decoded != null) entries.push({ label: "decoded", ts: decoded });
		const rendered = this.videoRenderer.out.timestamp.peek();
		if (rendered != null) entries.push({ label: "rendered", ts: rendered });
		return entries;
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
		this.#target.set({ pixels });
	}

	#runVideoEnabled(videoEnabled: Moq.Signals.Signal<boolean>, effect: Moq.Signals.Effect) {
		const exp = effect.get(this.expanded);
		const visible = effect.get(this.videoRenderer.out.visible);
		// On the grid or when expanded, but never download an off-screen tile.
		videoEnabled.set((exp === undefined || exp === this.sessionId) && visible);
	}

	#runAudioPaused(audioPaused: Moq.Signals.Signal<boolean>, effect: Moq.Signals.Effect) {
		const active = effect.get(this.active);
		audioPaused.set(!active);
	}

	#runStatus(effect: Moq.Signals.Effect) {
		const active = effect.get(this.broadcast.out.active);
		if (!active) return;

		const statusTrack = active.subscribe("status", { priority: 10 });
		effect.cleanup(() => statusTrack.close());

		// Reconstruct each status from snapshots and deltas, validated against the schema.
		const consumer = new Json.Snapshot.Consumer(statusTrack, { schema: GameStatusSchema });

		// Closing the track on cleanup unblocks a pending next() (it returns undefined), so the loop
		// ends without racing effect.cancel.
		effect.spawn(async () => {
			for (;;) {
				let status: GameStatus | undefined;
				try {
					status = await consumer.next();
				} catch (err) {
					console.warn("Invalid status JSON:", err);
					continue;
				}
				if (!status) break;

				this.status.set(status);
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
			this.#command.set(undefined);
			return;
		}

		const active = effect.get(this.#feedbackActive);
		if (!active) return;

		const viewerId = Math.random().toString(36).slice(2, 8);
		this.viewerId.set(viewerId);

		const viewerBroadcast = new Moq.Broadcast.Producer();
		conn.publish(Moq.Path.from(`${this.#viewerPrefix}/${this.sessionId}/${viewerId}`), viewerBroadcast);
		effect.cleanup(() => {
			viewerBroadcast.close();
			this.viewerId.set(undefined);
		});

		effect.spawn(async () => {
			for (;;) {
				const req = await Promise.race([effect.cancel, viewerBroadcast.requested()]);
				if (!req) break;

				if (req.name === "command") {
					// accept() commits the track's immutable properties and returns the producer.
					const track = req.accept();
					const producer = new Json.Snapshot.Producer<Record<string, unknown>>(track);
					effect.cleanup(() => producer.finish());
					effect.run(this.#runCommandTrack.bind(this, track, producer));
				}
			}
		});
	}

	#runCommandTrack(
		track: Moq.Track.Producer,
		producer: Json.Snapshot.Producer<Record<string, unknown>>,
		effect: Moq.Signals.Effect,
	) {
		if (effect.get(track.closed) !== undefined) return;

		const command = effect.get(this.#command);
		if (!command) return;

		producer.update({ ...command.cmd, timestamps: command.timestamps });
	}
}
