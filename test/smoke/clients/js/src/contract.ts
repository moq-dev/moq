/**
 * Everything the page and its driver both have to agree on.
 *
 * Free of browser imports on purpose: the drivers run under Bun, where importing anything that
 * reaches `@moq/publish` or `@moq/watch` fails on a browser-only asset (a worklet, a worker). Values
 * a driver needs live here; the page-side modules add the behavior.
 *
 * @module
 */

// ── the deterministic publisher ─────────────────────────────────────────────

/** A deliberate defect, used to prove an assertion can fail. */
export type Fault =
	/** Publish the pattern faithfully. */
	| "none"
	/** Mute the tone, leaving the audio track encoding digital silence. */
	| "silent-audio"
	/** Keep painting frame 0, so the picture never advances. */
	| "frozen-video"
	/** Run the tone table ahead of the picture by {@link OFFSET_STEPS}. */
	| "audio-offset";

/** Every recognized {@link Fault}, for argument validation. */
export const FAULTS: readonly Fault[] = ["none", "silent-audio", "frozen-video", "audio-offset"];

/** How far `audio-offset` shifts the tone table. Larger than the tolerance, smaller than half a cycle. */
export const OFFSET_STEPS = 4;

/** What the fixture publishes about itself, mirrored onto its host element's dataset. */
export type FixtureState = {
	/** The frame counter most recently painted, or -1 before the clock starts. */
	frameId: number;
	/** `AudioContext.state`. The pattern clock cannot start until this is "running". */
	audioState: AudioContextState;
	/** True once the broadcast is announced with both a video and an audio config in its catalog. */
	ready: boolean;
	/** True while a subscriber is pulling video, i.e. the encoder's demand gate is open. */
	videoActive: boolean;
	/** True while a subscriber is pulling audio. */
	audioActive: boolean;
	/** Frames the encoder has produced. */
	encodedFrames: number;
};

/** Rate the tone is generated and captured at. Stated rather than probed, so the catalog is fixed. */
export const SAMPLE_RATE = 48000;

// ── the subscriber's measurements ───────────────────────────────────────────

/** How often the page takes a sample. Fast enough to see a 200ms tone step, cheap enough to sustain. */
export const SAMPLE_MS = 50;

/** How far the tone peak must stand above the spectrum's median for the tone to count as present. */
export const TONE_FLOOR_DB = 15;

/**
 * Waveform level below which the playback sink counts as silent.
 *
 * The fixture's tone reaches the graph root at about 0.35 rms, so this is an order of magnitude of
 * headroom. It exists because a dB margin alone cannot tell silence from a tone: with most of the
 * spectrum at -Infinity, any finite peak stands infinitely above the floor.
 */
export const AUDIBLE_RMS = 0.02;

/** Live instances of each resource the page's wrappers count. */
export type Resources = {
	/** Open `WebTransport` sessions, i.e. connections to the relay. */
	transports: number;
	/** Open `WebSocket`s: the transport a connection falls back to when QUIC loses the race. */
	sockets: number;
	/** Unclosed `AudioContext`s, i.e. audio graphs and their render threads. */
	audioContexts: number;
	/** Unterminated `Worker`s. */
	workers: number;
};

/** One measurement of both playback sinks, taken in a single tick. */
export type Sample = {
	/** Monotonic counter, so a driver can tell a fresh sample from a repeat of the last one. */
	seq: number;
	/** `performance.now()` when the sample was taken. */
	at: number;

	/** Whether the canvas holds anything but the renderer's black fill. */
	painted: boolean;
	/** The fixture frame counter read out of the canvas, absent when the picture is not the fixture. */
	frameId?: number;
	/** Gap between the fixture's reference blocks, 0-255. Absent when the picture is not the fixture. */
	contrast?: number;
	/** Frames the decoder has produced. */
	videoFrames: number;
	/** Presentation timestamp of the painted frame, in milliseconds. */
	videoTimestamp?: number;

	/** Whether the catalog offers audio at all. */
	hasAudio: boolean;
	/** Encoded audio bytes received. */
	audioBytes: number;
	/** `AudioContext.state`, absent until the graph exists. */
	audioContext?: string;
	/** Playback timestamp reported by the render worklet, in milliseconds. */
	audioTimestamp?: number;
	/** Whether the audio buffer is waiting to refill. */
	audioStalled: boolean;
	/** Peak frequency in the tone band, absent until the graph exists. */
	toneHz?: number;
	/** The tone step that peak names, absent when no tone stands above the floor. */
	toneStep?: number;
	/** Level of the tone peak, in dB. */
	toneDb?: number;
	/** Median level across the spectrum, in dB: the floor the tone has to beat. */
	noiseDb?: number;
	/** Root mean square of the graph root's output over the analyser window. */
	rms?: number;

	/** Whether the player is paused. */
	paused: boolean;
	/** Whether the `paused` attribute is reflected onto the element. */
	pausedAttribute: boolean;

	/** Platform resources the page still holds. See {@link Resources}. */
	resources: Resources;
};

// ── the command channel ─────────────────────────────────────────────────────

/**
 * Commands a page publishes, each role providing the subset that applies to it.
 *
 * Each one is a DOM edit plus the bookkeeping that has to happen with it, which is why the page
 * owns them rather than the driver reaching in.
 */
export type SmokeControl = {
	/** Stop the fixture publisher, releasing its session. */
	stop(): void;
	/** Start (or restart) the fixture publisher on the same broadcast path. */
	start(): void;
	/** Remove the player from the DOM. */
	detach(): void;
	/** Put the player back and resume sampling. */
	reattach(): void;
	/** Forget to detach the player: the leaked-session negative control. */
	detachLeaky(): void;
};

/** The `window` property the commands are published on. */
export const CONTROL = "__moqSmoke";

/** Publish the commands a role supports. Page-side only. */
export function publish(commands: Partial<SmokeControl>): void {
	(window as unknown as Record<string, unknown>)[CONTROL] = commands;
}
