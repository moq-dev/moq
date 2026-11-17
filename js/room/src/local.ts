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
	/** The camera's switch. */
	camera?: {
		/** Capture the camera. Pass a Signal to share it with the app (hang.live Settings). Defaults to false. */
		enabled?: GetterInit<boolean>;
	};
	/** The microphone's switch. */
	microphone?: {
		/** Capture the microphone. Pass a Signal to share it with the app. Defaults to false. */
		enabled?: GetterInit<boolean>;
	};
	/** The screen share's switch. */
	screen?: {
		/** Prompt for and capture a screen; turns off when the share ends. Pass a Signal to share it with the app. Defaults to false. */
		enabled?: GetterInit<boolean>;
	};
	/** Seed the published user.json fields. */
	user?: UserProps;
}

/**
 * Local camera and screen broadcasts for one participant.
 *
 * Enable {@link enabled} to join (announce `{identity}/camera.hang`). Camera and
 * microphone capture are separate knobs; the screenshare broadcast is
 * announced only while joined and a share is live, so nothing is published
 * before joining or after leaving.
 */
export class Local {
	/** Participant identity. */
	readonly identity: Getter<Moq.Path.Valid>;

	/** Announce the camera broadcast (join the room). */
	readonly enabled: Signal<boolean>;

	/** The camera: its switch, device, capture, and the broadcast it publishes. */
	readonly camera: Local.Camera;
	/** The microphone: its switch, device, and gain. It publishes on the camera's broadcast. */
	readonly microphone: Local.Microphone;
	/** The screen share: its switch, picker, capture, and the broadcast it publishes. */
	readonly screen: Local.Screen;

	/** True while the local participant is composing a chat message. */
	readonly typing: Signal<boolean>;
	/** True while a chat message is live. */
	readonly chatting: Signal<boolean>;

	/** Published user.json fields. */
	readonly user: ReturnType<typeof userFields>;

	#preview = new Signal<Preview>({});
	#cameraVideo = new Signal<Publish.Video.Source | undefined>(undefined);
	#cameraAudio = new Signal<Publish.Audio.Source | undefined>(undefined);
	#screenVideo = new Signal<Publish.Video.Source | undefined>(undefined);
	#screenAudioSource = new Signal<Publish.Audio.Source | undefined>(undefined);
	#screenLive = new Signal(false);

	// The screen broadcast and its encoders run only while joined and sharing.
	#screenPublished = new Signal(false);

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
		const cameraEnabled = this.#control(props.camera?.enabled);
		const microphoneEnabled = this.#control(props.microphone?.enabled);
		const screenEnabled = this.#control(props.screen?.enabled);
		this.typing = new Signal(false);
		this.chatting = new Signal(false);
		this.user = userFields(props.user);

		const origin = props.connection.origin;
		const bandwidth = props.connection.allocator;

		const webcam = new Publish.Source.Camera({
			enabled: cameraEnabled,
			constraints: {
				width: { ideal: 1280 },
				height: { ideal: 720 },
				frameRate: { ideal: 30 },
				facingMode: { ideal: "user" },
			},
		});
		this.#signals.cleanup(() => webcam.close());

		const mic = new Publish.Source.Microphone({
			enabled: microphoneEnabled,
			constraints: {
				channelCount: { ideal: 1, max: 2 },
				autoGainControl: { ideal: true },
				noiseSuppression: { ideal: true },
				echoCancellation: { ideal: true },
			},
		});
		this.#signals.cleanup(() => mic.close());

		const share = new Publish.Source.Screen({
			enabled: screenEnabled,
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
		this.#signals.cleanup(() => share.close());
		this.#signals.run((effect) => {
			effect.set(this.#cameraVideo, effect.get(webcam.out.source)?.video);
			effect.set(this.#cameraAudio, effect.get(mic.out.source)?.audio);
		});

		const cameraCapture = new Publish.Video.Capture({
			source: this.#cameraVideo,
			orientation: webcam.out.orientation,
		});
		this.#signals.cleanup(() => cameraCapture.close());

		const screenCapture = new Publish.Video.Capture({ source: this.#screenVideo });
		this.#signals.cleanup(() => screenCapture.close());

		const cameraName = new Signal(broadcastPath(this.identity.peek(), KIND.camera));
		const screenName = new Signal(broadcastPath(this.identity.peek(), KIND.screen));
		this.#signals.run((effect) => {
			const identity = effect.get(this.identity);
			cameraName.set(broadcastPath(identity, KIND.camera));
			screenName.set(broadcastPath(identity, KIND.screen));
		});

		const cameraBroadcast = new Publish.Broadcast({
			origin,
			enabled: this.enabled,
			name: cameraName,
			display: cameraCapture.out.display,
			flip: true,
			rotation: cameraCapture.out.rotation,
		});
		this.#signals.cleanup(() => cameraBroadcast.close());

		this.#signals.run((effect) => {
			this.#screenPublished.set(effect.get(this.enabled) && effect.get(this.#screenLive));
		});

		const screenBroadcast = new Publish.Broadcast({
			origin,
			enabled: this.#screenPublished,
			name: screenName,
			display: screenCapture.out.display,
		});
		this.#signals.cleanup(() => screenBroadcast.close());

		const cameraHd = new Publish.Video.Encoder("video/hd", {
			broadcast: cameraBroadcast,
			capture: cameraCapture,
			enabled: cameraEnabled,
			bandwidth,
			config: { maxPixels: 1280 * 720 },
		});
		this.#signals.cleanup(() => cameraHd.close());

		const cameraSd = new Publish.Video.Encoder("video/sd", {
			broadcast: cameraBroadcast,
			capture: cameraCapture,
			enabled: cameraEnabled,
			bandwidth,
			config: { maxPixels: 640 * 360 },
		});
		this.#signals.cleanup(() => cameraSd.close());

		const microphoneCapture = new Publish.Audio.Capture({
			source: this.#cameraAudio,
			enabled: microphoneEnabled,
		});
		this.#signals.cleanup(() => microphoneCapture.close());

		// A muted microphone stays in the catalog and its listeners stay subscribed, so the next word
		// reaches them at once; its capture is still released.
		const microphoneMuted = new Signal(!microphoneEnabled.peek());
		this.#signals.run((effect) => microphoneMuted.set(!effect.get(microphoneEnabled)));
		const microphoneAudio = new Publish.Audio.Encoder("audio", {
			broadcast: cameraBroadcast,
			capture: microphoneCapture,
			muted: microphoneMuted,
			bandwidth,
		});
		this.#signals.cleanup(() => microphoneAudio.close());

		const screenHd = new Publish.Video.Encoder("video/hd", {
			broadcast: screenBroadcast,
			capture: screenCapture,
			enabled: this.#screenPublished,
			bandwidth,
			config: { maxPixels: 1920 * 1080 },
		});
		this.#signals.cleanup(() => screenHd.close());

		const screenSd = new Publish.Video.Encoder("video/sd", {
			broadcast: screenBroadcast,
			capture: screenCapture,
			enabled: this.#screenPublished,
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
			broadcast: screenBroadcast,
			capture: screenAudioCapture,
			enabled: this.#screenPublished,
			bandwidth,
		});
		this.#signals.cleanup(() => screenAudio.close());

		this.camera = { enabled: cameraEnabled, source: webcam, capture: cameraCapture, broadcast: cameraBroadcast };
		this.microphone = {
			enabled: microphoneEnabled,
			source: mic,
			volume: microphoneAudio.volume,
			root: microphoneCapture.out.root,
		};
		this.screen = {
			enabled: screenEnabled,
			source: share,
			capture: screenCapture,
			broadcast: screenBroadcast,
			audio: { root: screenAudioCapture.out.root },
		};

		this.#signals.run((effect) => {
			const state = effect.get(share.out.state);
			const media = state.kind === "live" ? state.media : undefined;
			effect.set(this.#screenVideo, media?.video);
			effect.set(this.#screenAudioSource, media?.audio);
			this.#screenLive.set(media !== undefined);
			// A share that ended for any reason, including a dismissed picker, turns sharing off, so the
			// next share takes a new toggle. `screen.source.out.state` keeps the reason.
			if (state.kind === "ended") screenEnabled.set(false);
		});

		this.#signals.run((effect) => {
			this.#preview.set({
				video: !!effect.get(webcam.out.source)?.video,
				audio: !!effect.get(mic.out.source)?.audio,
				screen: effect.get(this.#screenLive),
				name: effect.get(this.user.name),
				avatar: effect.get(this.user.avatar),
				chat: effect.get(this.chatting),
				typing: effect.get(this.typing),
			});
		});

		serve(cameraBroadcast, this.user, this.#preview, this.#signals);
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

/** Types on {@link Local}: one per device, each holding that device's switch and what it feeds. */
export namespace Local {
	/** The camera: its switch, device, capture, and broadcast. */
	export interface Camera {
		/** Capture the camera. */
		readonly enabled: Signal<boolean>;
		/** The camera device. */
		readonly source: Publish.Source.Camera;
		/** Shared capture feeding the hd and sd renditions. */
		readonly capture: Publish.Video.Capture;
		/** Camera+mic broadcast at `{identity}/camera.hang`. */
		readonly broadcast: Publish.Broadcast;
	}

	/** The microphone: its switch, device, and gain. It publishes on the camera's broadcast. */
	export interface Microphone {
		/** Capture the microphone. Off mutes the rendition rather than removing it. */
		readonly enabled: Signal<boolean>;
		/** The microphone device. */
		readonly source: Publish.Source.Microphone;
		/** Linear gain applied before encoding, where 1 is unity. */
		readonly volume: Signal<number>;
		/** Raw microphone graph while capture is running; its context may render silently. */
		readonly root: Getter<AudioNode | undefined>;
	}

	/** The screen share: its switch, picker, capture, broadcast, and audio. */
	export interface Screen {
		/** Prompt for and capture a screen. Turns off when the share ends, and `source.out.state` says why. */
		readonly enabled: Signal<boolean>;
		/** The screen picker and its share. */
		readonly source: Publish.Source.Screen;
		/** Shared capture feeding the hd and sd renditions. */
		readonly capture: Publish.Video.Capture;
		/** Screenshare broadcast at `{identity}/screen.hang`. */
		readonly broadcast: Publish.Broadcast;
		/** The share's audio. */
		readonly audio: {
			/** Raw screen audio graph while capture is running; its context may render silently. */
			readonly root: Getter<AudioNode | undefined>;
		};
	}
}
