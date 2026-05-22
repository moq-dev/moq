// The kind of audio being encoded. Drives Opus application/signal settings on the encoder.
// - "voice": speech (microphone). Opus application=voip + signal=voice.
// - "music": music or mixed content (screen/tab capture). Opus application=audio + signal=music.
// - "auto": let the encoder decide. Opus defaults (good for unknown sources like file playback).
export type Kind = "voice" | "music" | "auto";

// A bare track is accepted for backwards compatibility and treated as kind="auto".
// Prefer the { track, kind } object form so the encoder can pick the right Opus settings.
export type Source = StreamTrack | SourceConfig;

export interface SourceConfig {
	track: StreamTrack;
	kind: Kind;
}

export function normalizeSource(source: Source): SourceConfig {
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
