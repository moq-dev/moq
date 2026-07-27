/**
 * Where video comes from: a live capture track, or a stream of frames we produce ourselves (a
 * decoded file, an image, a canvas you drive yourself).
 */
export type Source = StreamTrack | FrameSource;

/**
 * A stream of frames from something that isn't a capture device.
 *
 * Frames are delivered in presentation order and are owned by the reader, which closes each one.
 * Stamp their timestamps against `performance.now() * 1000`, the same epoch capture uses, so audio
 * and video stay in sync.
 */
export interface FrameSource {
	/** The frames to encode. Cancelled when the source is dropped. */
	frames: ReadableStream<VideoFrame>;

	/** The nominal frame rate, if known. Feeds the bitrate calculation and the encoder config. */
	frameRate?: number;
}

/** Whether this source is a live capture track rather than a {@link FrameSource}. */
export function isStreamTrack(source: Source): source is StreamTrack {
	// Structural check rather than `instanceof MediaStreamTrack` so this stays correct across realms.
	return !("frames" in source);
}

// Stronger typing for the MediaStreamTrack interface.
export interface StreamTrack extends MediaStreamTrack {
	kind: "video";
	clone(): StreamTrack;
	getSettings(): TrackSettings;
}

export interface TrackSettings {
	deviceId: string;
	groupId: string;

	// I'm not sure what fields are always present.
	aspectRatio: number;
	facingMode: "user" | "environment" | "left" | "right";
	frameRate?: number;
	height: number;
	resizeMode: "none" | "crop-and-scale";
	width: number;
}

export type Constraints = Omit<
	MediaTrackConstraints,
	"autoGainControl" | "channelCount" | "echoCancellation" | "noiseSuppression" | "sampleRate" | "sampleSize"
> & {
	// TODO update @types/web
	resizeMode?: "none" | "crop-and-scale";
};
