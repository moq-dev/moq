export type Source = StreamTrack;

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
