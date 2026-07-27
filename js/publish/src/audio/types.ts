// The kind of audio being encoded. Drives the default Opus application/signal/DTX settings on the
// encoder, each of which an explicit OpusConfig can still override.
// - "voice": speech (microphone). Opus application=voip, signal=voice, and DTX on (speech has gaps).
// - "music": music or mixed content (screen/tab capture). Opus application=audio + signal=music.
// - "auto": let the encoder decide. Opus defaults (good for unknown sources like file playback).
export type Kind = "voice" | "music" | "auto";

// Where audio comes from: a live capture track, or a stream of samples we decoded ourselves.
// A bare track is accepted for backwards compatibility and treated as kind="auto".
// Prefer the { track, kind } object form so the encoder can pick the right Opus settings.
export type Source = StreamTrack | SourceConfig | SampleSource;

export interface SourceConfig {
	track: StreamTrack;
	kind: Kind;
}

/**
 * A stream of samples from something that isn't a capture device, e.g. a demuxed file.
 *
 * Samples are delivered in presentation order and consumed by the encoder, which closes each one.
 * Stamp their timestamps against `performance.now() * 1000`, the same epoch capture uses, so audio
 * and video stay in sync.
 */
export interface SampleSource {
	/** The samples to encode. Cancelled when the source is dropped. */
	samples: ReadableStream<AudioData>;

	/** The rate the samples were decoded at, in Hz. The encoder resamples if the codec can't carry it. */
	sampleRate: number;

	/** The channel count the samples were decoded at. */
	channelCount: number;

	/** What kind of audio these samples carry, driving the Opus defaults. */
	kind: Kind;
}

/** Whether this source is a stream of decoded samples rather than a live capture track. */
export function isSampleSource(source: Source): source is SampleSource {
	// Structural check rather than `instanceof MediaStreamTrack` so this stays correct across realms.
	return "samples" in source;
}

/** What kind of audio the source carries. A bare track says nothing, so it's "auto". */
export function sourceKind(source: Source): Kind {
	return "track" in source || "samples" in source ? source.kind : "auto";
}

/** Resolve the bare-track shorthand to its full config form. */
export function normalizeSource(source: StreamTrack | SourceConfig): SourceConfig {
	// Structural check rather than `instanceof MediaStreamTrack` so this stays correct across realms.
	return "track" in source ? source : { track: source, kind: "auto" };
}

export type Constraints = Omit<
	MediaTrackConstraints,
	"aspectRatio" | "backgroundBlur" | "displaySurface" | "facingMode" | "frameRate" | "height" | "width"
>;

// Stronger typing for the MediaStreamTrack interface.
export interface StreamTrack extends MediaStreamTrack {
	kind: "audio";
	clone(): StreamTrack;
	getSettings(): TrackSettings;
}

// MediaTrackSettings can represent both audio and video, which means a LOT of possibly undefined properties.
// This is a fork of the MediaTrackSettings interface with properties required for audio or video.
export interface TrackSettings {
	deviceId: string;
	groupId: string;

	// Seems to be available on all browsers.
	sampleRate: number;

	// The rest is optional unfortunately.
	autoGainControl?: boolean;
	channelCount?: number; // ugh Safari why
	echoCancellation?: boolean;
	noiseSuppression?: boolean;
	sampleSize?: number;
}
