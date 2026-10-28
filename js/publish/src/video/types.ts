/**
 * Where video comes from: a live capture track, or a stream of frames we produce ourselves (a
 * decoded file, an image, a canvas you drive yourself).
 */
export type Source = StreamTrack | SourceConfig | FrameSource;

/** A video track and its native pixels per logical pixel. */
export interface SourceConfig {
	/** The track to capture. */
	track: StreamTrack;
	/** Native pixels per logical pixel; sampled per frame, so a property getter can provide live values. */
	scale?: number;
}

/** Return the track and sizing options for either capture source form. */
export function normalizeSource(source: StreamTrack | SourceConfig): SourceConfig {
	return "track" in source ? source : { track: source };
}

/**
 * A stream of frames from something that isn't a capture device.
 *
 * Frames are delivered in presentation order and are owned by the reader, which closes each one.
 * Stamp their timestamps against `@moq/clock`'s `now()` in microseconds, the same epoch capture uses, so audio
 * and video stay in sync.
 */
export interface FrameSource {
	/** The frames to encode. Cancelled when the source is dropped. */
	frames: ReadableStream<VideoFrame>;

	/** The nominal frame rate, if known. Feeds the bitrate calculation and the encoder config. */
	frameRate?: number;
}

/** Whether this source is a bare live capture track. */
export function isStreamTrack(source: Source): source is StreamTrack {
	// Structural check rather than `instanceof MediaStreamTrack` so this stays correct across realms.
	return !("frames" in source) && !("track" in source);
}

/** Stronger typing for the MediaStreamTrack interface. */
export interface StreamTrack extends MediaStreamTrack {
	/** Always video. */
	kind: "video";
	/** A new track on the same source, with its own settings and lifetime. */
	clone(): StreamTrack;
	/** The settings the track currently runs with. */
	getSettings(): TrackSettings;
}

/** The settings a video track reports, with the fields video always has made required. */
export interface TrackSettings {
	/** The capture device. */
	deviceId: string;
	/** The physical device the capture device belongs to. */
	groupId: string;

	// I'm not sure what fields are always present.
	/** Width divided by height. */
	aspectRatio: number;
	/** Which way a camera faces. */
	facingMode: "user" | "environment" | "left" | "right";
	/** The frames per second, when known. */
	frameRate?: number;
	/** The height in pixels. */
	height: number;
	/** Whether the browser crops or scales to meet the constraints. */
	resizeMode: "none" | "crop-and-scale";
	/** The width in pixels. */
	width: number;
}

/** The `getUserMedia` constraints that apply to video. */
export type Constraints = Omit<
	MediaTrackConstraints,
	"autoGainControl" | "channelCount" | "echoCancellation" | "noiseSuppression" | "sampleRate" | "sampleSize"
> & {
	// TODO update @types/web
	resizeMode?: "none" | "crop-and-scale";
};
