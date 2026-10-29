# `@moq/publish`

```ts
export namespace Audio {
	export type Aac = "aac" | Audio.AacConfig;
	export type AacConfig = {
		bitrate?: number;
		mime: "aac";
	};
	export interface AudioFrame {
		channels: Float32Array[];
		timestamp: Clock.Micro;
	}
	export class Capture {
		constructor(props?: Audio.CaptureProps);
		channelCount: Signals.Signal<number | undefined>;
		close(): void;
		readonly in: Signals.Readonlys<Audio.CaptureInput>;
		readonly out: Signals.Readonlys<Audio.CaptureOutput>;
		sampleRate: Signals.Signal<number | undefined>;
	}
	export type CaptureInput = {
		enabled: Signals.Getter<boolean>;
		source: Signals.Getter<Audio.Source | undefined>;
	};
	export type CaptureOutput = {
		format: Signals.Signal<Audio.Format | undefined>;
		frames: Signals.Signal<Fanout<Audio.AudioFrame> | undefined>;
		root: Signals.Signal<AudioNode | undefined>;
	};
	export type CaptureProps = Signals.Inputs<Audio.CaptureInput> & {
		channelCount?: number | Signals.Signal<number | undefined>;
		sampleRate?: number | Signals.Signal<number | undefined>;
	};
	export type Codec = Audio.Opus | Audio.Aac;
	export type CodecMime = "opus" | "aac";
	export type Constraints = Omit<MediaTrackConstraints, "aspectRatio" | "backgroundBlur" | "displaySurface" | "facingMode" | "frameRate" | "height" | "width">;
	export class Encoder {
		constructor(name: string, props?: Audio.EncoderProps);
		get capture(): Audio.Capture | undefined;
		close(): void;
		codec: Signals.Signal<Audio.Codec>;
		groupDuration: Signals.Signal<Clock.Milli>;
		readonly in: Signals.Readonlys<Audio.EncoderInput>;
		readonly name: string;
		readonly out: Signals.Readonlys<Audio.EncoderOutput>;
		volume: Signals.Signal<number>;
	}
	export type EncoderInput = {
		bandwidth: Signals.Getter<Net.Bandwidth.Handle | undefined>;
		broadcast: Signals.Getter<Broadcast | undefined>;
		capture: Signals.Getter<Audio.Capture | undefined>;
		enabled: Signals.Getter<boolean>;
	};
	export type EncoderOutput = {
		active: Signals.Signal<boolean>;
		catalog: Signals.Signal<Hang.Catalog.Audio.AudioConfig | undefined>;
		error: Signals.Signal<Error | undefined>;
		root: Signals.Signal<AudioNode | undefined>;
		stats: Signals.Signal<Audio.Stats>;
	};
	export type EncoderProps = Signals.Inputs<Audio.EncoderInput> & {
		codec?: Audio.Codec | Signals.Signal<Audio.Codec>;
		groupDuration?: Clock.Milli | Signals.Signal<Clock.Milli>;
		volume?: number | Signals.Signal<number>;
	};
	export interface Format {
		channelCount: number;
		sampleRate: number;
	}
	export type Kind = "voice" | "music" | "auto";
	export type Opus = "opus" | Audio.OpusConfig;
	export type OpusConfig = {
		bitrate?: number;
		complexity?: number;
		frameDuration?: Clock.Milli;
		mime: "opus";
		packetlossperc?: number;
		useinbandfec?: boolean;
	};
	export type Rendition = import("@moq/publish").Rendition<Hang.Catalog.Audio.AudioConfig>;
	export interface SampleSource {
		channelCount: number;
		kind: Audio.Kind;
		sampleRate: number;
		samples: ReadableStream<AudioData>;
	}
	export type Source = Audio.StreamTrack | Audio.SourceConfig | Audio.SampleSource;
	export interface SourceConfig {
		kind: Audio.Kind;
		track: Audio.StreamTrack;
	}
	export interface Stats {
		bytes: number;
		frames: number;
	}
	export interface StreamTrack extends MediaStreamTrack {
		clone(): Audio.StreamTrack;
		getSettings(): Audio.TrackSettings;
		kind: "audio";
	}
	export interface TrackSettings {
		autoGainControl?: boolean;
		channelCount?: number;
		deviceId: string;
		echoCancellation?: boolean;
		groupId: string;
		noiseSuppression?: boolean;
		sampleRate: number;
		sampleSize?: number;
	}
	export function isSampleSource(source: Audio.Source): source is Audio.SampleSource;
	export function normalizeSource(source: Audio.StreamTrack | Audio.SourceConfig): Audio.SourceConfig;
	export function sourceKind(source: Audio.Source): Audio.Kind;
}

export class Broadcast {
	constructor(props?: Signals.Inputs<BroadcastInput>);
	static readonly CATALOG_TRACK: "catalog.json";
	audio(name: string): Rendition<Hang.Catalog.Audio.AudioConfig>;
	readonly catalog: CatalogProducer;
	close(): void;
	readonly in: Signals.Readonlys<BroadcastInput>;
	readonly net: Signals.Getter<Net.Broadcast.Producer | undefined>;
	text(name: string): Rendition<Hang.Catalog.TextConfig>;
	video(name: string): Rendition<Hang.Catalog.Video.VideoConfig>;
}

export type BroadcastInput = {
	announce: Signals.Getter<boolean>;
	display: Signals.Getter<{
		height: number;
		width: number;
	} | undefined>;
	enabled: Signals.Getter<boolean>;
	flip: Signals.Getter<boolean>;
	maxAge: Signals.Getter<Clock.Milli | undefined>;
	name: Signals.Getter<Net.Path.Valid>;
	origin: Signals.Getter<Net.Origin.Table | undefined>;
	rotation: Signals.Getter<number>;
};

export class CatalogProducer {
	constructor();
	mutate(fn: (catalog: Hang.Catalog.Root) => void): void;
	serve(track: Net.Track.Producer, effect: Signals.Effect): void;
}

export * as Clock from "@moq/clock";

export class Fanout<T> {
	constructor(source: ReadableStream<T>, props?: FanoutProps<T>);
	close(): void;
	subscribe(effect: Signals.Effect, queue?: number): ReadableStream<T>;
}

export interface FanoutProps<T> {
	clone?: (value: T) => T;
	queue?: number;
	release?: (value: T) => void;
}

export * as Hang from "@moq/hang";

export type Kind = "video" | "audio" | "text";

export * as Net from "@moq/net";

export namespace Preview {
	export { Backend } from "@moq/video";
	export type Mode = "none" | "source" | "encoded";
	export class Renderer {
		constructor(props?: Preview.RendererProps);
		close(): void;
		readonly in: Signals.Readonlys<Preview.RendererInput>;
		readonly out: Signals.Readonlys<{
			error: Signals.Getter<Error | undefined>;
		}>;
	}
	export type RendererInput = {
		backend: Signals.Getter<Preview.Backend>;
		canvas: Signals.Getter<HTMLCanvasElement | undefined>;
		display: Signals.Getter<{
			height: number;
			width: number;
		} | undefined>;
		enabled: Signals.Getter<boolean>;
		encoder: Signals.Getter<Video.Encoder | undefined>;
		flip: Signals.Getter<boolean>;
		frames: Signals.Getter<Fanout<VideoFrame> | undefined>;
		mode: Signals.Getter<Preview.Mode>;
		rotation: Signals.Getter<number>;
	};
	export type RendererProps = Signals.Inputs<Preview.RendererInput>;
	export class Transcode {
		constructor(props?: Preview.TranscodeProps);
		close(): void;
		readonly in: Signals.Readonlys<Preview.TranscodeInput>;
		readonly out: Signals.Readonlys<Preview.TranscodeOutput>;
	}
	export type TranscodeInput = {
		config: Signals.Getter<VideoEncoderConfig | undefined>;
		settings: Signals.Getter<Video.Config | undefined>;
		source: Signals.Getter<Fanout<VideoFrame> | undefined>;
	};
	export type TranscodeOutput = {
		frame: Signals.Signal<VideoFrame | undefined>;
	};
	export type TranscodeProps = Signals.Inputs<Preview.TranscodeInput>;
}

export class Rendition<Config> {
	constructor();
	close(): void;
	readonly config: Signals.Signal<Config | undefined>;
	readonly kind: Kind;
	readonly name: string;
	readonly track: Signals.Getter<Net.Track.Producer | undefined>;
}

export * as Signals from "@moq/signals";

export namespace Source {
	export class Camera {
		constructor(props?: Source.CameraProps);
		static readonly DEFAULT_CONSTRAINTS: Video.Constraints;
		close(): void;
		constraints: Signals.Signal<Video.Constraints | undefined>;
		readonly device: Source.Device<"video">;
		readonly in: Signals.Readonlys<Source.CameraInput>;
		readonly out: Signals.Readonlys<Source.CameraOutput>;
	}
	export type CameraInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type CameraOutput = {
		error: Signals.Signal<Error | undefined>;
		orientation: Signals.Signal<number>;
		source: Signals.Signal<Source.Media | undefined>;
	};
	export interface CameraProps extends Signals.Inputs<Source.CameraInput> {
		constraints?: Video.Constraints | Signals.Signal<Video.Constraints | undefined>;
		device?: Source.DeviceProps;
	}
	export class Device<Kind extends "audio" | "video"> {
		constructor(kind: Kind, props?: Source.DeviceProps);
		capture(deviceId: string | undefined): Signals.Dispose;
		close(): void;
		readonly in: Signals.Readonlys<Source.DeviceInput>;
		readonly kind: Kind;
		readonly out: Signals.Readonlys<Source.DeviceOutput>;
		preferred: Signals.Signal<string | undefined>;
		requestPermission(): void;
	}
	export type DeviceInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type DeviceOutput = {
		active: Signals.Signal<string | undefined>;
		available: Signals.Signal<MediaDeviceInfo[] | undefined>;
		default: Signals.Signal<string | undefined>;
		permission: Signals.Signal<boolean>;
		requested: Signals.Signal<string | undefined>;
	};
	export interface DeviceProps extends Signals.Inputs<Source.DeviceInput> {
		preferred?: string | Signals.Signal<string | undefined>;
	}
	export class File {
		constructor(props?: Source.FileProps);
		close(): void;
		file: Signals.Signal<globalThis.File | undefined>;
		readonly in: Signals.Readonlys<Source.FileInput>;
		readonly out: Signals.Readonlys<Source.FileOutput>;
		prompt(): void;
	}
	export type FileInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type FileOutput = {
		source: Signals.Signal<Source.Media | undefined>;
	};
	export interface FileProps extends Signals.Inputs<Source.FileInput> {
		file?: globalThis.File | Signals.Signal<globalThis.File | undefined>;
	}
	export type Media = {
		audio?: Audio.Source;
		video?: Video.Source;
	};
	export class Microphone {
		constructor(props?: Source.MicrophoneProps);
		close(): void;
		constraints: Signals.Signal<Audio.Constraints | undefined>;
		readonly device: Source.Device<"audio">;
		readonly in: Signals.Readonlys<Source.MicrophoneInput>;
		readonly out: Signals.Readonlys<Source.MicrophoneOutput>;
	}
	export type MicrophoneInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type MicrophoneOutput = {
		error: Signals.Signal<Error | undefined>;
		source: Signals.Signal<Source.Media | undefined>;
	};
	export interface MicrophoneProps extends Signals.Inputs<Source.MicrophoneInput> {
		constraints?: Audio.Constraints | Signals.Signal<Audio.Constraints | undefined>;
		device?: Source.DeviceProps;
	}
	export class Screen {
		constructor(props?: Source.ScreenProps);
		audio: Signals.Signal<Audio.Constraints | boolean | undefined>;
		close(): void;
		readonly in: Signals.Readonlys<Source.ScreenInput>;
		readonly out: Signals.Readonlys<Source.ScreenOutput>;
		video: Signals.Signal<Video.Constraints | boolean | undefined>;
	}
	export type ScreenInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type ScreenOutput = {
		source: Signals.Signal<Source.Media | undefined>;
	};
	export interface ScreenProps extends Signals.Inputs<Source.ScreenInput> {
		audio?: Audio.Constraints | boolean | Signals.Signal<Audio.Constraints | boolean | undefined>;
		video?: Video.Constraints | boolean | Signals.Signal<Video.Constraints | boolean | undefined>;
	}
}

export namespace Video {
	export class Capture {
		constructor(props?: Signals.Inputs<Video.CaptureInput>);
		close(): void;
		readonly in: Signals.Readonlys<Video.CaptureInput>;
		readonly out: Signals.Readonlys<Video.CaptureOutput>;
	}
	export type CaptureInput = {
		orientation: Signals.Getter<number>;
		source: Signals.Getter<Video.Source | undefined>;
	};
	export type CaptureOutput = {
		display: Signals.Signal<{
			height: number;
			scale?: number;
			width: number;
		} | undefined>;
		frames: Signals.Signal<Fanout<VideoFrame> | undefined>;
		rotation: Signals.Signal<number>;
	};
	export interface Config {
		bitrateScale?: number;
		codec?: string;
		frameRate?: number;
		keyframeInterval?: Clock.Milli;
		maxBitrate?: number;
		maxPixels?: number;
		maxScale?: number;
	}
	export type Constraints = Omit<MediaTrackConstraints, "autoGainControl" | "channelCount" | "echoCancellation" | "noiseSuppression" | "sampleRate" | "sampleSize"> & {
		resizeMode?: "none" | "crop-and-scale";
	};
	export class Encoder {
		constructor(name: string, props?: Video.EncoderProps);
		get capture(): Video.Capture | undefined;
		close(): void;
		config: Signals.Signal<Video.Config | undefined>;
		cut(): void;
		readonly in: Signals.Readonlys<Video.EncoderInput>;
		readonly name: string;
		readonly out: Signals.Readonlys<Video.EncoderOutput>;
	}
	export type EncoderInput = {
		bandwidth: Signals.Getter<Net.Bandwidth.Handle | undefined>;
		broadcast: Signals.Getter<Broadcast | undefined>;
		capture: Signals.Getter<Video.Capture | undefined>;
		enabled: Signals.Getter<boolean>;
	};
	export type EncoderOutput = {
		active: Signals.Signal<boolean>;
		catalog: Signals.Signal<Hang.Catalog.Video.VideoConfig | undefined>;
		error: Signals.Signal<Error | undefined>;
		resolved: Signals.Signal<VideoEncoderConfig | undefined>;
		stats: Signals.Signal<Video.Stats>;
	};
	export type EncoderProps = Signals.Inputs<Video.EncoderInput> & {
		config?: Video.Config | Signals.Signal<Video.Config | undefined>;
	};
	export interface FrameSource {
		frameRate?: number;
		frames: ReadableStream<VideoFrame>;
	}
	export type Rendition = import("@moq/publish").Rendition<Hang.Catalog.Video.VideoConfig>;
	export type Source = Video.StreamTrack | Video.SourceConfig | Video.FrameSource;
	export interface SourceConfig {
		scale?: number;
		track: Video.StreamTrack;
	}
	export interface Stats {
		bytes: number;
		frames: number;
		keyframes: number;
	}
	export interface StreamTrack extends MediaStreamTrack {
		clone(): Video.StreamTrack;
		getSettings(): Video.TrackSettings;
		kind: "video";
	}
	export interface TrackSettings {
		aspectRatio: number;
		deviceId: string;
		facingMode: "user" | "environment" | "left" | "right";
		frameRate?: number;
		groupId: string;
		height: number;
		resizeMode: "none" | "crop-and-scale";
		width: number;
	}
	export function isStreamTrack(source: Video.Source): source is Video.StreamTrack;
	export function normalizeSource(source: Video.StreamTrack | Video.SourceConfig): Video.SourceConfig;
}

export function assets(url: string | URL): void;
```
