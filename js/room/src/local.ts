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

type Established = Moq.Connection.Established;

/** Constructor options for {@link Local}. */
export interface LocalProps {
	/** Live session, usually a `Connection.Reload`'s `established`. */
	connection: GetterInit<Established | undefined>;
	/** Participant identity; broadcast names are `{identity}/camera` and `{identity}/screen`. */
	identity: GetterInit<Moq.Path.Valid>;
	/** When true, announce the camera broadcast (joining the room). Defaults to false. */
	enabled?: boolean | Signal<boolean>;
	/** Seed the published user.json fields. */
	user?: UserProps;
}

/**
 * Local camera and screen broadcasts for one participant.
 *
 * Enable {@link enabled} to join (announce `{identity}/camera`). Camera and
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

	/** Published user.json fields. */
	readonly user: ReturnType<typeof userFields>;

	/** Camera capture source. */
	readonly webcam: Publish.Source.Camera;
	/** Microphone capture source. */
	readonly microphone: Publish.Source.Microphone;
	/** Screen capture source. */
	readonly share: Publish.Source.Screen;

	/** Camera+mic broadcast at `{identity}/camera`. */
	readonly camera: Publish.Broadcast;
	/** Screenshare broadcast at `{identity}/screen`. */
	readonly screen: Publish.Broadcast;

	/** Shared capture feeding the camera renditions. */
	readonly cameraCapture: Publish.Video.Capture;
	/** Shared capture feeding the screen renditions. */
	readonly screenCapture: Publish.Video.Capture;

	/** Camera HD encoder. */
	readonly cameraHd: Publish.Video.Encoder;
	/** Camera SD encoder. */
	readonly cameraSd: Publish.Video.Encoder;
	/** Camera microphone encoder. */
	readonly cameraAudio: Publish.Audio.Encoder;

	/** Screen HD encoder. */
	readonly screenHd: Publish.Video.Encoder;
	/** Screen SD encoder. */
	readonly screenSd: Publish.Video.Encoder;
	/** Screen audio encoder (tab/system audio when the share includes it). */
	readonly screenAudio: Publish.Audio.Encoder;

	#preview = new Signal<Preview>({});
	#screenVideo = new Signal<Publish.Video.Source | undefined>(undefined);
	#screenAudioSource = new Signal<Publish.Audio.Source | undefined>(undefined);
	#screenLive = new Signal(false);

	#signals = new Effect();

	constructor(props: LocalProps) {
		this.identity = getter(props.identity);
		this.enabled = Signal.from(props.enabled ?? false);
		this.cameraEnabled = new Signal(false);
		this.microphoneEnabled = new Signal(false);
		this.screenEnabled = new Signal(false);
		this.user = userFields(props.user);

		const connection = getter(props.connection);

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

		this.cameraCapture = new Publish.Video.Capture({ source: this.webcam.out.source });
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
			connection,
			enabled: this.enabled,
			name: cameraName,
			display: this.cameraCapture.out.display,
			flip: true,
		});
		this.#signals.cleanup(() => this.camera.close());

		this.screen = new Publish.Broadcast({
			connection,
			enabled: this.#screenLive,
			name: screenName,
			display: this.screenCapture.out.display,
		});
		this.#signals.cleanup(() => this.screen.close());

		this.cameraHd = new Publish.Video.Encoder("video/hd", {
			broadcast: this.camera,
			capture: this.cameraCapture,
			enabled: this.cameraEnabled,
			config: { maxPixels: 1280 * 720 },
		});
		this.#signals.cleanup(() => this.cameraHd.close());

		this.cameraSd = new Publish.Video.Encoder("video/sd", {
			broadcast: this.camera,
			capture: this.cameraCapture,
			enabled: this.cameraEnabled,
			config: { maxPixels: 640 * 360 },
		});
		this.#signals.cleanup(() => this.cameraSd.close());

		this.cameraAudio = new Publish.Audio.Encoder("audio", {
			broadcast: this.camera,
			source: this.microphone.out.source,
			enabled: this.microphoneEnabled,
		});
		this.#signals.cleanup(() => this.cameraAudio.close());

		this.screenHd = new Publish.Video.Encoder("video/hd", {
			broadcast: this.screen,
			capture: this.screenCapture,
			enabled: this.#screenLive,
			config: { maxPixels: 1920 * 1080 },
		});
		this.#signals.cleanup(() => this.screenHd.close());

		this.screenSd = new Publish.Video.Encoder("video/sd", {
			broadcast: this.screen,
			capture: this.screenCapture,
			enabled: this.#screenLive,
			config: { maxPixels: 960 * 540 },
		});
		this.#signals.cleanup(() => this.screenSd.close());

		this.screenAudio = new Publish.Audio.Encoder("audio", {
			broadcast: this.screen,
			source: this.#screenAudioSource,
			enabled: this.#screenLive,
		});
		this.#signals.cleanup(() => this.screenAudio.close());

		this.#signals.run((effect) => {
			const source = effect.get(this.share.out.source);
			this.#screenVideo.set(source?.video);
			this.#screenAudioSource.set(source?.audio);
			const live = !!source?.video || !!source?.audio;
			this.#screenLive.set(live);
			if (!live && effect.get(this.screenEnabled)) {
				this.screenEnabled.set(false);
			}
		});

		this.#signals.run((effect) => {
			this.#preview.set({
				video: !!effect.get(this.webcam.out.source),
				audio: !!effect.get(this.microphone.out.source),
				screen: effect.get(this.#screenLive),
				name: effect.get(this.user.name),
				avatar: effect.get(this.user.avatar),
			});
		});

		serve(this.camera, this.user, this.#preview, this.#signals);
	}

	/** Latest preview.json value this participant is publishing. */
	get preview(): Getter<Preview> {
		return this.#preview;
	}

	close() {
		this.#signals.close();
	}
}
