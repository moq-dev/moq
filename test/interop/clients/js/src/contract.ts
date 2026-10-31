/**
 * Everything the page and its driver both have to agree on.
 *
 * Free of browser imports on purpose: the drivers run under Bun, where importing anything that
 * reaches `@moq/publish` or `@moq/watch` fails on a browser-only asset (a worklet, a worker). Values
 * a driver needs live here; the page-side modules add the behavior.
 *
 * @module
 */
import type { Broadcast } from "@moq/watch";

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
	/** The page's `Date.now()` when {@link ready} first held, so a driver can time from it without its poll lag. */
	readyAt?: number;
	/** True while a subscriber is pulling video, i.e. the encoder's demand gate is open. */
	videoActive: boolean;
	/** True while a subscriber is pulling audio. */
	audioActive: boolean;
	/** Frames the encoder has produced. */
	encodedFrames: number;
	/** Whether the video rendition is published as enabled; false while {@link InteropControl.disableVideo} holds. */
	videoEnabled: boolean;
	/** The audio renditions the catalog advertises, by name, with the format each one encodes. */
	audio: Record<string, { codec: string; sampleRate: number; numberOfChannels: number }>;
};

/** The codecs the fixture can encode its tone in, by the publisher's codec name. */
export const AUDIO_CODECS = ["opus", "aac"] as const;
/** One of {@link AUDIO_CODECS}. */
export type AudioCodec = (typeof AUDIO_CODECS)[number];

/** The rate {@link InteropControl.changeAudioRate} moves the capture to: one Opus carries, under the default. */
export const CHANGED_SAMPLE_RATE = 24000;

/** The name of the second audio rendition {@link InteropControl.addAudioRendition} publishes. */
export const ALT_AUDIO = "audio-alt";

/** The camera publisher state mirrored onto its element for Playwright. */
export type CaptureState = {
	videoError?: string;
	audioError?: string;
	videoActive: boolean;
	audioActive: boolean;
};

/** Rate the tone is generated and captured at. Stated rather than probed, so the catalog is fixed. */
export const SAMPLE_RATE = 48000;

/** Maximum distance between video keyframes in the deterministic fixture. */
export const KEYFRAME_INTERVAL_MS = 500;

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

/**
 * The interop matrix's tone check, the in-page twin of `test/interop/tone.py`: each channel must
 * carry at least this level and this share of its energy at its own tone. Keep the two in step.
 */
export const TONE_CHECK = { minRms: 0.05, minShare: 0.8 };

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

/**
 * True once a leaked player has started, even when it reuses a pooled transport.
 *
 * Two `<moq-watch>` elements on one relay URL share a WebTransport, so a session
 * count cannot move. Each player still builds its own audio graph.
 */
export function leakedPlayerStarted(before: Resources, now: Resources): boolean {
	return now.audioContexts > before.audioContexts;
}

/** One measurement of both playback sinks, taken in a single tick. */
export type Sample = {
	/** Monotonic counter, so a driver can tell a fresh sample from a repeat of the last one. */
	seq: number;
	/** Whether the subscriber has resolved an announced broadcast. */
	broadcastActive: boolean;
	/** The active route epoch, absent on legacy sessions. */
	broadcastEpoch?: string;
	/** The subscriber's catalog state, which stays offline without an announcement. */
	broadcastStatus: ReturnType<Broadcast["out"]["status"]["peek"]>;
	/** Whether this document has received user activation. */
	userActivated: boolean;
	/** `performance.now()` when the sample was taken. */
	at: number;

	/** Whether the canvas holds anything but the renderer's black fill. */
	painted: boolean;
	/** The fixture frame counter read out of the canvas, absent when the picture is not the fixture. */
	frameId?: number;
	/** Source generation read from the presented pixels. */
	generation?: number;
	/** Presented canvas dimensions, independent of the received catalog. */
	frameWidth?: number;
	frameHeight?: number;
	/** Coded dimensions of the decoded frame supplied to the renderer. */
	videoCodedWidth?: number;
	videoCodedHeight?: number;
	/** The catalog's resolved video track and codec. */
	videoTrack?: string;
	videoCodec?: string;
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
	/** How far playback trails the live edge, in milliseconds: the player's resolved sync delay. */
	delay: number;
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
	/** The codec of the audio rendition the player selected. */
	audioCodec?: string;
	/** The selected audio rendition's channel count. */
	audioChannels?: number;
	/** The selected audio rendition's sample rate. */
	audioSampleRate?: number;
	/** The selected audio rendition's catalog name. */
	audioRendition?: string;
	/** The track the selected audio rendition is served on, which a format change re-mints. */
	audioTrack?: string;
	/**
	 * Root mean square of what reaches `AudioContext.destination`, after the player's volume and mute.
	 * Absent until the graph exists; zero while nothing is connected to the speakers.
	 */
	speakerRms?: number;
	/**
	 * Per channel of the graph root, when the page was given the expected tones: the channel's RMS
	 * and the share of its energy at its own tone, over the last ~170ms it played.
	 */
	tones?: { rms: number; share: number }[];

	/** `aria-label` of the primary control button: "Pause" while playing, "Play" while paused. */
	controlLabel?: string;
	/** Whether the big center play button is showing. */
	centerPlayVisible: boolean;

	/** Whether the player is paused. */
	paused: boolean;
	/** Whether the `paused` attribute is reflected onto the element. */
	pausedAttribute: boolean;

	/** Platform resources the page still holds. See {@link Resources}. */
	resources: Resources;
};

/** A main-thread stall a page logged: one long animation frame and the scripts that ran longest in it. */
export type LongFrame = {
	/** When the frame started, in epoch milliseconds, so two pages' logs line up. */
	at: number;
	/** How long the frame took, in milliseconds. */
	duration: number;
	/** How much of it blocked input, in milliseconds. */
	blocking: number;
	/** The longest-running scripts in the frame, or "no script" when the time went elsewhere. */
	scripts: string;
};

/** The `window` property each page keeps its {@link LongFrame} log on. */
export const LONG_FRAMES = "__moqLongFrames";

/** Whether a changed presented frame belongs to playback within the live bound. */
export function reachesLive(observation: {
	previous?: number;
	source: number;
	sample: Pick<Sample, "frameId" | "audioContext" | "audioStalled">;
	bound: number;
}): boolean {
	const { previous, source, sample, bound } = observation;
	return (
		sample.audioContext === "running" &&
		!sample.audioStalled &&
		previous !== undefined &&
		sample.frameId !== undefined &&
		sample.frameId !== previous &&
		source - sample.frameId <= bound
	);
}

// ── the refused session ─────────────────────────────────────────────────────

/** How a refused session ended, from `WebTransport.closed` or `Connection.error`. */
export type CloseState =
	/** The peer's session close code and description. */
	| { closeCode: number; reason: string }
	/** `closed` rejected, or the relay admitted the session: the close never said why. */
	| { error: string };

// ── the command channel ─────────────────────────────────────────────────────

/**
 * Commands a page publishes, each role providing the subset that applies to it.
 *
 * Each one is a DOM edit plus the bookkeeping that has to happen with it, which is why the page
 * owns them rather than the driver reaching in.
 */
export type InteropControl = {
	/** Stop the fixture publisher, releasing its session. */
	stop(): void;
	/** Start (or restart) the fixture publisher on the same broadcast path. */
	start(): void;
	/** Remove the player from the DOM. */
	detach(): void;
	/** Blank the canvas the old session left behind, put the player back, and resume sampling. */
	reattach(): void;
	/** Connect a second player and leave it behind for the leaked-session negative control. */
	startLeak(): void;
	/** Keep the fixture's video rendition in the catalog with `enabled: false`, stopping its frames. */
	disableVideo(): void;
	/** Re-enable the fixture's video rendition. */
	enableVideo(): void;
	/** Switch the fixture from AVC to VP8 without replacing its broadcast. */
	changeVideoCodec(): void;
	/** Double the fixture's coded dimensions while keeping its VP8 identity. */
	resizeVideo(): void;
	/** Switch the fixture's audio rendition from Opus to AAC without replacing its broadcast. */
	changeAudioCodec(): void;
	/** Capture and encode the tone at {@link CHANGED_SAMPLE_RATE} instead of {@link SAMPLE_RATE}. */
	changeAudioRate(): void;
	/** Capture and encode the tone in stereo instead of mono. */
	changeAudioChannels(): void;
	/** Publish a second audio rendition, {@link ALT_AUDIO}, encoding the same capture. */
	addAudioRendition(): void;
	/** Stop publishing {@link ALT_AUDIO}. */
	removeAudioRendition(): void;
};

/** The `window` property the commands are published on. */
export const CONTROL = "__moqInterop";

/** Publish the commands a role supports. Page-side only. */
export function publish(commands: Partial<InteropControl>): void {
	(window as unknown as Record<string, unknown>)[CONTROL] = commands;
}

/** Prefix of the console line carrying each player {@link Sample} to the driver. */
export const SAMPLE_EVENT = "__moqInteropSample:";

/**
 * Prefix of the console line carrying the fixture's {@link FixtureState}, or `null` once it closes.
 * Publishers report over the console like players do, so the driver reads any engine without
 * evaluating in the page, which would grant it user activation.
 */
export const FIXTURE_EVENT = "__moqInteropFixture:";

/** Prefix of the console line carrying the capture publisher's {@link CaptureState}. */
export const CAPTURE_EVENT = "__moqInteropCapture:";
