/**
 * The local participant: camera+mic and screenshare publishers.
 *
 * @module
 */

import type * as Moq from "@moq/net";
import * as Publish from "@moq/publish";
import { Effect, type Getter, type GetterInit, getter, Signal } from "@moq/signals";
import { type Preview, serve, type UserProps, userFields } from "./metadata.ts";
import { broadcastPath, KIND } from "./path.ts";

/** Constructor options for {@link Local}. */
export interface LocalProps {
	/** Reconnecting connection to publish through. */
	connection: Moq.Connection;
	/** Participant identity; broadcast names are `{identity}/camera.hang` and `{identity}/screen.hang`. */
	identity: GetterInit<Moq.Path.Valid>;
	/** When true, announce the camera broadcast (joining the room). Defaults to false. */
	enabled?: GetterInit<boolean>;
	/** Capture the camera. Pass a Signal to share it with the app (hang.live Settings). */
	cameraEnabled?: GetterInit<boolean>;
	/** Capture the microphone. Pass a Signal to share it with the app. */
	microphoneEnabled?: GetterInit<boolean>;
	/** Prompt for and capture a screen. Pass a Signal to share it with the app. */
	screenEnabled?: GetterInit<boolean>;
	/** Seed the published user.json fields. */
	user?: UserProps;
}

/**
 * Local camera and screen broadcasts for one participant.
 *
 * Enable {@link enabled} to join (announce `{identity}/camera.hang`). Camera and
 * microphone capture are separate knobs; the screenshare broadcast is
 * announced only while a share is live.
 */
export class Local {
	/** Participant identity. */
	readonly identity: Getter<Moq.Path.Valid>;

	/** Announce the camera broadcast (join the room). */
	readonly enabled: Signal<boolean>;

	/** Capture the camera. */
	readonly cameraEnabled: Signal<boolean>;
	/** Capture the microphone. */
	readonly microphoneEnabled: Signal<boolean>;
	/** Prompt for and capture a screen. Unannounce when the share ends. */
	readonly screenEnabled: Signal<boolean>;

	/** True while the local participant is composing a chat message. */
	readonly typing: Signal<boolean>;
	/** True while a chat message is live. */
	readonly chatting: Signal<boolean>;

	/** Published user.json fields. */
	readonly user: ReturnType<typeof userFields>;

	/** Camera capture source. */
	readonly webcam: Publish.Source.Camera;
	/** Microphone capture source. */
	readonly microphone: Publish.Source.Microphone;
	/** Screen capture source. */
	readonly share: Publish.Source.Screen;

	/** Camera+mic broadcast at `{identity}/camera.hang`. */
	readonly camera: Publish.Broadcast;
	/** Screenshare broadcast at `{identity}/screen.hang`. */
	readonly screen: Publish.Broadcast;

	/** Shared capture feeding the camera renditions. */
	readonly cameraCapture: Publish.Video.Capture;
	/** Shared capture feeding the screen renditions. */
	readonly screenCapture: Publish.Video.Capture;
	#preview = new Signal<Preview>({});
	#cameraVideo = new Signal<Publish.Video.Source | undefined>(undefined);
	#cameraAudio = new Signal<Publish.Audio.Source | undefined>(undefined);
	#screenVideo = new Signal<Publish.Video.Source | undefined>(undefined);
	#screenAudioSource = new Signal<Publish.Audio.Source | undefined>(undefined);
	#screenLive = new Signal(false);

	#signals = new Effect();

	#control(value: GetterInit<boolean> | undefined): Signal<boolean> {
		const input = getter(value ?? false);
		if (input instanceof Signal) return input;

		const output = new Signal(input.peek());
		this.#signals.proxy(output, input);
		return output;
	}

	constructor(props: LocalProps) {
		this.identity = getter(props.identity);
		this.enabled = this.#control(props.enabled);
		this.cameraEnabled = this.#control(props.cameraEnabled);
		this.microphoneEnabled = this.#control(props.microphoneEnabled);
		this.screenEnabled = this.#control(props.screenEnabled);
		this.typing = new Signal(false);
		this.chatting = new Signal(false);
		this.user = userFields(props.user);

		const origin = props.connection.origin;
		const bandwidth = props.connection.bandwidth;

		this.webcam = new Publish.Source.Camera({
			enabled: this.cameraEnabled,
			constraints: {
				width: { ideal: 1280 },
				height: { ideal: 720 },
				frameRate: { ideal: 30 },
				facingMode: { ideal: "user" },
			},
		});
		this.#signals.cleanup(() => this.webcam.close());

		this.microphone = new Publish.Source.Microphone({
			enabled: this.microphoneEnabled,
			constraints: {
				channelCount: { ideal: 1, max: 2 },
				autoGainControl: { ideal: true },
				noiseSuppression: { ideal: true },
				echoCancellation: { ideal: true },
			},
		});
		this.#signals.cleanup(() => this.microphone.close());

		this.share = new Publish.Source.Screen({
			enabled: this.screenEnabled,
			video: {
				frameRate: { ideal: 30 },
				width: { max: 1920 },
				height: { max: 1080 },
			},
			audio: {
				channelCount: { ideal: 2, max: 2 },
				autoGainControl: { ideal: false },
				echoCancellation: { ideal: false },
				noiseSuppression: { ideal: false },
			},
		});
		this.#signals.cleanup(() => this.share.close());
		this.#signals.run((effect) => {
			effect.set(this.#cameraVideo, effect.get(this.webcam.out.source)?.video);
			effect.set(this.#cameraAudio, effect.get(this.microphone.out.source)?.audio);
		});

		this.cameraCapture = new Publish.Video.Capture({ source: this.#cameraVideo });
		this.#signals.cleanup(() => this.cameraCapture.close());

		this.screenCapture = new Publish.Video.Capture({ source: this.#screenVideo });
		this.#signals.cleanup(() => this.screenCapture.close());

		const cameraName = new Signal(broadcastPath(this.identity.peek(), KIND.camera));
		const screenName = new Signal(broadcastPath(this.identity.peek(), KIND.screen));
		this.#signals.run((effect) => {
			const identity = effect.get(this.identity);
			cameraName.set(broadcastPath(identity, KIND.camera));
			screenName.set(broadcastPath(identity, KIND.screen));
		});

		this.camera = new Publish.Broadcast({
			origin,
			enabled: this.enabled,
			name: cameraName,
			display: this.cameraCapture.out.display,
			flip: true,
		});
		this.#signals.cleanup(() => this.camera.close());

		this.screen = new Publish.Broadcast({
			origin,
			enabled: this.#screenLive,
			name: screenName,
			display: this.screenCapture.out.display,
		});
		this.#signals.cleanup(() => this.screen.close());

		const cameraHd = new Publish.Video.Encoder("video/hd", {
			broadcast: this.camera,
			capture: this.cameraCapture,
			enabled: this.cameraEnabled,
			bandwidth,
			config: { maxPixels: 1280 * 720 },
		});
		this.#signals.cleanup(() => cameraHd.close());

		const cameraSd = new Publish.Video.Encoder("video/sd", {
			broadcast: this.camera,
			capture: this.cameraCapture,
			enabled: this.cameraEnabled,
			bandwidth,
			config: { maxPixels: 640 * 360 },
		});
		this.#signals.cleanup(() => cameraSd.close());

		const cameraAudioCapture = new Publish.Audio.Capture({
			source: this.#cameraAudio,
			enabled: this.microphoneEnabled,
		});
		this.#signals.cleanup(() => cameraAudioCapture.close());

		const cameraAudio = new Publish.Audio.Encoder("audio", {
			broadcast: this.camera,
			capture: cameraAudioCapture,
			enabled: this.microphoneEnabled,
			bandwidth,
		});
		this.#signals.cleanup(() => cameraAudio.close());

		const screenHd = new Publish.Video.Encoder("video/hd", {
			broadcast: this.screen,
			capture: this.screenCapture,
			enabled: this.#screenLive,
			bandwidth,
			config: { maxPixels: 1920 * 1080 },
		});
		this.#signals.cleanup(() => screenHd.close());

		const screenSd = new Publish.Video.Encoder("video/sd", {
			broadcast: this.screen,
			capture: this.screenCapture,
			enabled: this.#screenLive,
			bandwidth,
			config: { maxPixels: 960 * 540 },
		});
		this.#signals.cleanup(() => screenSd.close());

		const screenAudioCapture = new Publish.Audio.Capture({
			source: this.#screenAudioSource,
			enabled: this.#screenLive,
		});
		this.#signals.cleanup(() => screenAudioCapture.close());

		const screenAudio = new Publish.Audio.Encoder("audio", {
			broadcast: this.screen,
			capture: screenAudioCapture,
			enabled: this.#screenLive,
			bandwidth,
		});
		this.#signals.cleanup(() => screenAudio.close());

		this.#signals.run((effect) => {
			const source = effect.get(this.share.out.source);
			effect.set(this.#screenVideo, source?.video);
			effect.set(this.#screenAudioSource, source?.audio);
			const live = !!source?.video || !!source?.audio;
			const wasLive = this.#screenLive.peek();
			this.#screenLive.set(live);
			if (!live && wasLive) {
				this.screenEnabled.set(false);
			}
		});

		this.#signals.run((effect) => {
			this.#preview.set({
				video: !!effect.get(this.webcam.out.source)?.video,
				audio: !!effect.get(this.microphone.out.source)?.audio,
				screen: effect.get(this.#screenLive),
				name: effect.get(this.user.name),
				avatar: effect.get(this.user.avatar),
				chat: effect.get(this.chatting),
				typing: effect.get(this.typing),
			});
		});

		serve(this.camera, this.user, this.#preview, this.#signals);
	}

	/** Latest preview.json value this participant is publishing. */
	get preview(): Getter<Preview> {
		return this.#preview;
	}

	/** Release this participant's subscriptions and media resources. */
	close() {
		this.#signals.close();
	}
}
