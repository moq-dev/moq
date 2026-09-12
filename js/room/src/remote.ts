/**
 * A remote participant: camera and screen watch pipelines plus metadata.
 *
 * @module
 */

import type * as Moq from "@moq/net";
import { Effect, type Getter, type GetterInit, getter, type Readonlys, readonlys, Signal } from "@moq/signals";
import * as Watch from "@moq/watch";
import { consume, type Preview, type UserInput } from "./metadata.ts";
import { KIND, type Kind } from "./path.ts";

type Established = Moq.Connection.Established;

/** One watched broadcast (camera or screen) for a remote participant. */
export class Member {
	/** `camera` or `screen`. */
	readonly kind: Kind;
	/** Broadcast path relative to the connection root. */
	readonly path: Moq.Path.Valid;

	/** Canvas to paint into; assign from the app. */
	readonly canvas = new Signal<HTMLCanvasElement | undefined>(undefined);
	/** Mute this member's audio; defaults to true. */
	readonly muted = new Signal(true);
	/** Playback volume, 0..1. */
	readonly volume = new Signal(0.5);

	/** Watched broadcast and catalog. */
	readonly broadcast: Watch.Broadcast;
	/** Video decoding pipeline. */
	readonly video: Watch.Video.Decoder;
	/** Audio decoding pipeline. */
	readonly audio: Watch.Audio.Decoder;
	/** Canvas video renderer. */
	readonly renderer: Watch.Video.Renderer;
	/** Speaker audio output. */
	readonly emitter: Watch.Audio.Emitter;

	/** Published participant identity and display fields. */
	readonly user: Readonlys<UserInput>;
	/** Published presence fields. */
	readonly preview: Getter<Preview>;

	#videoEnabled = new Signal(false);
	#audioEnabled = new Signal(false);
	#metadata: ReturnType<typeof consume>;
	#signals = new Effect();

	constructor(kind: Kind, path: Moq.Path.Valid, connection: Getter<Established | undefined>) {
		this.kind = kind;
		this.path = path;

		this.broadcast = new Watch.Broadcast({
			connection,
			enabled: true,
			name: path,
			reload: true,
		});
		this.#signals.cleanup(() => this.broadcast.close());

		const videoSource = new Watch.Video.Source({
			broadcast: this.broadcast,
			supported: Watch.Video.Decoder.supported,
		});
		const audioSource = new Watch.Audio.Source({
			broadcast: this.broadcast,
			supported: Watch.Audio.Decoder.supported,
		});
		this.#signals.cleanup(() => {
			videoSource.close();
			audioSource.close();
		});

		const sync = new Watch.Sync({
			latency: "real-time",
			connection,
			video: videoSource.out.jitter,
			audio: audioSource.out.jitter,
		});
		this.#signals.cleanup(() => sync.close());

		this.video = new Watch.Video.Decoder(videoSource, sync, { enabled: this.#videoEnabled });
		this.audio = new Watch.Audio.Decoder(audioSource, sync, { enabled: this.#audioEnabled });
		this.#signals.cleanup(() => {
			this.video.close();
			this.audio.close();
		});

		this.renderer = new Watch.Video.Renderer(this.video, {
			canvas: this.canvas,
		});
		this.emitter = new Watch.Audio.Emitter(this.audio, {
			volume: this.volume,
			muted: this.muted,
		});
		this.#signals.cleanup(() => {
			this.renderer.close();
			this.emitter.close();
		});

		this.#signals.run((effect) => {
			this.#videoEnabled.set(effect.get(this.renderer.out.visible));
		});
		this.#signals.run((effect) => {
			this.#audioEnabled.set(effect.get(this.emitter.out.enabled));
		});

		this.#metadata = consume(this.broadcast);
		this.user = this.#metadata.user;
		this.preview = this.#metadata.preview;
		this.#signals.cleanup(() => this.#metadata.close());
	}

	/** Release this participant's subscriptions and media resources. */
	close() {
		this.#signals.close();
	}
}

/** Constructor options for {@link Remote}. */
export interface RemoteProps {
	/** Participant identity. */
	identity: Moq.Path.Valid;
	/** Live session, usually a `Connection.Reload`'s `established`. */
	connection: GetterInit<Established | undefined>;
}

/**
 * Groups one identity's `camera` and `screen` broadcasts.
 *
 * User and preview metadata come from the camera broadcast when it is live,
 * otherwise from the screen.
 */
export class Remote {
	/** Participant identity. */
	readonly identity: Moq.Path.Valid;

	readonly #camera = new Signal<Member | undefined>(undefined);
	readonly #screen = new Signal<Member | undefined>(undefined);
	readonly #user = {
		id: new Signal<string | undefined>(undefined),
		name: new Signal<string | undefined>(undefined),
		avatar: new Signal<string | undefined>(undefined),
		color: new Signal<string | undefined>(undefined),
	};
	readonly #preview = new Signal<Preview>({});

	/** The live camera member, if announced. */
	readonly camera: Getter<Member | undefined>;
	/** The live screen member, if announced. */
	readonly screen: Getter<Member | undefined>;
	/** Published participant identity and display fields. */
	readonly user: Readonlys<UserInput>;
	/** Published presence fields. */
	readonly preview: Getter<Preview>;

	#connection: Getter<Established | undefined>;
	#signals = new Effect();

	constructor(props: RemoteProps) {
		this.identity = props.identity;
		this.#connection = getter(props.connection);
		this.camera = this.#camera;
		this.screen = this.#screen;
		this.user = readonlys(this.#user);
		this.preview = this.#preview;

		this.#signals.run((effect) => {
			const member = effect.get(this.#camera) ?? effect.get(this.#screen);
			if (!member) {
				this.#user.id.set(undefined);
				this.#user.name.set(undefined);
				this.#user.avatar.set(undefined);
				this.#user.color.set(undefined);
				this.#preview.set({});
				return;
			}
			this.#user.id.set(effect.get(member.user.id));
			this.#user.name.set(effect.get(member.user.name));
			this.#user.avatar.set(effect.get(member.user.avatar));
			this.#user.color.set(effect.get(member.user.color));
			this.#preview.set(effect.get(member.preview));
		});
	}

	/** Attach a live camera or screen broadcast. */
	attach(kind: Kind, path: Moq.Path.Valid): void {
		const slot = kind === KIND.camera ? this.#camera : this.#screen;
		slot.peek()?.close();
		slot.set(new Member(kind, path, this.#connection));
	}

	/** Detach a camera or screen broadcast that went offline. */
	detach(kind: Kind): void {
		const slot = kind === KIND.camera ? this.#camera : this.#screen;
		slot.peek()?.close();
		slot.set(undefined);
	}

	/** True when neither camera nor screen is live. */
	empty(): boolean {
		return !this.#camera.peek() && !this.#screen.peek();
	}

	/** Release this participant's subscriptions and media resources. */
	close() {
		this.#camera.peek()?.close();
		this.#screen.peek()?.close();
		this.#camera.set(undefined);
		this.#screen.set(undefined);
		this.#signals.close();
	}
}
