/**
 * The non-UI Game Boy session backend.
 *
 * Lives in its own module (not the package entry) so UI components can import
 * `Game`/`KEY_MAP` without pulling in `index.ts`, which re-exports the element.
 *
 * @module
 */
import { Duration } from "@moq/clock";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";
import { z } from "zod/mini";
import type { GameStatus, LatencyEntry } from "./schemas.ts";
import { GameStatusSchema, ReplySchema } from "./schemas.ts";

/** Configuration for creating a Game instance. */
export interface GameConfig {
	/** Unique session identifier (e.g. the ROM name). */
	sessionId: string;
	/** MoQ connection to the relay. */
	connection: Moq.Connection;
	/** The origin the game broadcast arrives in; the connection feeds it. */
	origin: Moq.Origin.Producer;
	/** This viewer's socket endpoint, under {@link gamePrefix}, which dials the game's. */
	endpoint: Moq.Socket.Endpoint;
	/** Shared signal tracking which game is currently expanded. */
	expanded: Moq.Signals.Signal<string | undefined>;
	/** MoQ path prefix for game broadcasts (e.g. "anon/boy/game"). */
	gamePrefix: string;
}

/** A command with captured timestamps, published via the command signal. */
interface Command {
	cmd: Record<string, unknown>;
	timestamps: { label: string; ts: number }[];
}

// Close the session to the emulator after 60s of no input.
const FEEDBACK_IDLE = Duration.secs(60);

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
	/** The session identifier, such as the ROM name. */
	readonly sessionId: string;
	readonly #signals = new Moq.Signals.Effect();

	/** The session currently expanded, shared by every game on the page. */
	readonly expanded: Moq.Signals.Signal<string | undefined>;
	readonly #endpoint: Moq.Socket.Endpoint;

	/** Whether the pointer is over this game's tile. */
	readonly hovered = new Moq.Signals.Signal(false);
	/** Whether this game is expanded or hovered, which enables audio and input. */
	readonly active = new Moq.Signals.Signal(false);
	/** The playback delay target. */
	readonly delay = new Moq.Signals.Signal<Watch.Delay>("auto");
	/** Whether the viewer muted this game. */
	readonly userMuted = new Moq.Signals.Signal(false);
	/** The playback volume, from 0 to 1. */
	readonly volume = new Moq.Signals.Signal(0.25);
	/** The latest status the emulator published, or undefined before the first. */
	readonly status = new Moq.Signals.Signal<GameStatus | undefined>(undefined);
	/** This viewer's latency breakdown, as the emulator last reported it while a session is open. */
	readonly latency = new Moq.Signals.Signal<LatencyEntry[] | undefined>(undefined);

	/** The canvas the video renders into, set by the UI once it is mounted. */
	readonly canvas = new Moq.Signals.Signal<HTMLCanvasElement | undefined>(undefined);

	// The video rendition target, owned here and wired into the video source as an input.
	readonly #target = new Moq.Signals.Signal<Watch.Video.Target | undefined>(undefined);

	/** The game's broadcast. */
	readonly broadcast: Watch.Broadcast;
	/** The clock that keeps video and audio in step. */
	readonly sync: Watch.Sync;
	/** Picks the video rendition to fetch. */
	readonly videoSource: Watch.Video.Source;
	/** Decodes the video rendition. */
	readonly videoDecoder: Watch.Video.Decoder;
	/** Draws decoded frames into {@link canvas}. */
	readonly videoRenderer: Watch.Video.Renderer;
	/** Picks the audio rendition to fetch. */
	readonly audioSource: Watch.Audio.Source;
	/** Decodes the audio rendition. */
	readonly audioDecoder: Watch.Audio.Decoder;
	/** Plays decoded audio. */
	readonly audioEmitter: Watch.Audio.Emitter;

	/** The Game Boy buttons currently held down. */
	readonly heldButtons = new Set<string>();

	// The command waiting for the session to open, consumed once written.
	#command = new Moq.Signals.Signal<Command | undefined>(undefined);
	readonly #connection: Moq.Connection;
	#feedbackActive = new Moq.Signals.Signal(false);

	constructor(config: GameConfig) {
		const { sessionId, connection, expanded, gamePrefix } = config;
		this.sessionId = sessionId;
		this.expanded = expanded;
		this.#endpoint = config.endpoint;
		this.#connection = connection;

		// Derive active state from expanded + hovered.
		this.#signals.run(this.#runActive.bind(this));

		// Video pipeline.
		this.broadcast = new Watch.Broadcast({
			origin: config.origin,
			name: Moq.Path.from(gamePrefix, sessionId),
			enabled: true,
		});
		this.#signals.cleanup(() => this.broadcast.close());

		this.videoSource = new Watch.Video.Source({
			broadcast: this.broadcast,
			target: this.#target,
			supported: Watch.Video.Decoder.supported,
			rate: new Moq.Signals.Derived([connection.probe], (probe) => probe?.estimatedRecvRate),
		});
		this.#signals.cleanup(() => this.videoSource.close());

		this.audioSource = new Watch.Audio.Source({
			broadcast: this.broadcast,
			supported: Watch.Audio.Decoder.supported,
		});
		this.#signals.cleanup(() => this.audioSource.close());

		this.sync = new Watch.Sync({ delay: this.delay });
		this.#signals.cleanup(() => this.sync.close());

		this.#signals.run(this.#runPixelBudget.bind(this));

		const videoEnabled = new Moq.Signals.Signal(true);
		this.videoDecoder = new Watch.Video.Decoder({
			source: this.videoSource,
			sync: this.sync,
			enabled: videoEnabled,
		});
		this.#signals.cleanup(() => this.videoDecoder.close());

		// Renderer needs a canvas created by the UI layer, set via `canvas`.
		this.videoRenderer = new Watch.Video.Renderer({ decoder: this.videoDecoder, canvas: this.canvas });
		this.#signals.cleanup(() => this.videoRenderer.close());

		// Download on the grid or when expanded, but only while the tile is on-screen.
		// Reads the renderer's visibility, so it must run after the renderer exists.
		this.#signals.run(this.#runVideoEnabled.bind(this, videoEnabled));

		// Audio pipeline. The emitter stops the download when muted or paused.
		const audioEnabled = new Moq.Signals.Signal(false);
		this.audioDecoder = new Watch.Audio.Decoder({
			source: this.audioSource,
			sync: this.sync,
			enabled: audioEnabled,
		});
		this.#signals.cleanup(() => this.audioDecoder.close());

		const audioPaused = new Moq.Signals.Signal(true);
		this.#signals.run(this.#runAudioPaused.bind(this, audioPaused));

		this.audioEmitter = new Watch.Audio.Emitter({
			source: this.audioDecoder,
			volume: this.volume,
			muted: this.userMuted,
			paused: audioPaused,
		});
		this.#signals.cleanup(() => this.audioEmitter.close());
		this.#signals.proxy(audioEnabled, this.audioEmitter.out.enabled);

		// Resume AudioContext on first user interaction (browser autoplay policy).
		for (const event of ["click", "touchstart", "touchend", "mousedown", "keydown"]) {
			this.#signals.event(document, event, () => {
				const ctx = this.audioDecoder.out.context.peek();
				if (ctx?.state === "suspended") ctx.resume();
			});
		}

		// Subscribe to status track.
		this.#signals.run(this.#runStatus.bind(this));

		// The session to the emulator, open while this viewer gives input.
		this.#signals.run(this.#runSession.bind(this, connection));
		this.#signals.run(this.#runFeedbackIdle.bind(this));
	}

	/** Send a button state update. */
	sendButtons() {
		this.sendCommand({ type: "buttons", buttons: [...this.heldButtons] });
	}

	/** Send a command to the emulator. Dropped while disconnected, since it would land late and out of context. */
	sendCommand(cmd: Record<string, unknown>) {
		if (this.#connection.status.peek() !== "connected") return;

		// Open the session on input, with idle timeout.
		this.#feedbackActive.set(true);

		this.#command.set({ cmd, timestamps: this.#timestamps() }, true);
	}

	/** Collect media timestamps, in milliseconds, at each pipeline stage for latency measurement. */
	#timestamps(): { label: string; ts: number }[] {
		const entries: { label: string; ts: number }[] = [];
		const received = this.sync.out.timestamp.peek();
		if (received != null) entries.push({ label: "received", ts: received.asMillis() });
		const decoded = this.videoDecoder.out.timestamp.peek();
		if (decoded != null) entries.push({ label: "decoded", ts: decoded.asMillis() });
		const rendered = this.videoRenderer.out.timestamp.peek();
		if (rendered != null) entries.push({ label: "rendered", ts: rendered.asMillis() });
		return entries;
	}

	/** Stop playback and input for this session. */
	close() {
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

		// Reconstruct each status from snapshots and deltas, validated against the schema.
		const consumer = new Json.Snapshot.Consumer({
			track: active.track("status"),
			schema: GameStatusSchema,
			priority: 10,
		});
		effect.cleanup(() => consumer.close());

		// Closing the consumer on cleanup unblocks a pending latest() (it returns undefined), so the loop
		// ends without racing the teardown.
		effect.spawn(async () => {
			for (;;) {
				let status: GameStatus | undefined;
				try {
					status = (await consumer.latest())?.value;
				} catch (err) {
					// The value was decoded and only failed the schema, so the next frame still applies.
					// Anything else, such as a track closed with an error, throws on every call: end the loop.
					if (!(err instanceof z.core.$ZodError)) throw err;
					console.warn("Invalid status:", err);
					continue;
				}
				if (!status) break;

				this.status.set(status);
			}
		});
	}

	#runFeedbackIdle(effect: Moq.Signals.Effect) {
		if (!effect.get(this.#feedbackActive)) return;
		// Every command restarts the idle timer.
		effect.get(this.#command);
		effect.timer(() => this.#feedbackActive.set(false), FEEDBACK_IDLE);
	}

	#runSession(connection: Moq.Connection, effect: Moq.Signals.Effect) {
		// Gate on a live connection, since a dial is answered only through the relay. Losing it or
		// deactivating drops pending input, which would otherwise replay on the next session.
		if (effect.get(connection.status) !== "connected" || !effect.get(this.active)) {
			this.#feedbackActive.set(false);
			this.#command.set(undefined);
			return;
		}

		if (!effect.get(this.#feedbackActive)) return;

		// One session per activation: a reload or an idle timeout ends it, and the next input dials a new one.
		const abort = new AbortController();
		const stream = new Moq.Signals.Signal<Moq.Socket.TrackStream | undefined>(undefined);
		effect.cleanup(() => {
			abort.abort();
			const open = stream.peek();
			open?.producer.finish();
			open?.consumer.close();
			this.latency.set(undefined);
		});

		effect.spawn(async () => {
			let open: Moq.Socket.TrackStream;
			try {
				open = await this.#endpoint.dialTrack(this.sessionId, undefined, { signal: abort.signal });
			} catch (err) {
				if (abort.signal.aborted) return;
				console.warn("Dial failed:", err);
				// Redial on the next input rather than dropping commands into no session.
				this.#feedbackActive.set(false);
				return;
			}
			if (abort.signal.aborted) {
				open.producer.finish();
				open.consumer.close();
				return;
			}
			stream.set(open);
			await this.#readReplies(open.consumer, abort.signal);
			// The emulator ended the session, so close this side and redial on the next input.
			if (!abort.signal.aborted) this.#feedbackActive.set(false);
		});

		effect.run(this.#runCommands.bind(this, stream));
	}

	#runCommands(stream: Moq.Signals.Signal<Moq.Socket.TrackStream | undefined>, effect: Moq.Signals.Effect) {
		const open = effect.get(stream);
		if (!open) return;

		const command = effect.get(this.#command);
		if (!command) return;

		// One command per group, so a late one never holds up the next.
		const value = { ...command.cmd, timestamps: command.timestamps };
		// The socket track is scale-less, so the frame carries its group's key.
		const group = open.producer.appendGroup();
		group.writeFrame({
			payload: new TextEncoder().encode(JSON.stringify(value)),
			timestamp: Moq.Time.Timestamp.unscaled(group.sequence),
		});
		group.finish();
		this.#command.set(undefined);
	}

	// Read the emulator's replies until the session ends.
	async #readReplies(consumer: Moq.Track.Subscriber, signal: AbortSignal) {
		const replies = consumer.ordered();
		try {
			for (;;) {
				const group = await replies.nextGroup();
				if (!group) return;

				let reply: unknown;
				try {
					reply = await group.readJson();
				} catch (err) {
					console.warn("Invalid reply:", err);
					continue;
				}
				const parsed = ReplySchema.safeParse(reply);
				if (!parsed.success) {
					console.warn("Invalid reply:", parsed.error);
					continue;
				}
				this.latency.set(parsed.data.latency);
			}
		} catch (err) {
			if (!signal.aborted) console.warn("Session ended:", err);
		}
	}
}
