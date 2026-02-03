export type Source = StreamTrack;

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
