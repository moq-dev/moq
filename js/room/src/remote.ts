/**
 * A remote participant: camera and screen watch pipelines plus metadata.
 *
 * @module
 */

import type * as Moq from "@moq/net";
import { Effect, type Getter, type Readonlys, readonlys, Signal } from "@moq/signals";
import * as Watch from "@moq/watch";
import { consume, type Preview, type UserInput } from "./metadata.ts";
import { KIND, type Kind } from "./path.ts";

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

	/** Playback pipeline for this member. */
	readonly player: Watch.Player;
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

	#metadata: ReturnType<typeof consume>;
	#signals = new Effect();

	constructor(kind: Kind, path: Moq.Path.Valid, connection: Moq.Connection) {
		this.kind = kind;
		this.path = path;

		this.player = new Watch.Player({
			origin: connection.origin,
			probe: connection.probe,
			name: path,
			canvas: this.canvas,
			muted: this.muted,
			volume: this.volume,
		});
		this.#signals.cleanup(() => this.player.close());
		this.broadcast = this.player.broadcast;
		this.video = this.player.video;
		this.audio = this.player.audio;
		this.renderer = this.player.renderer;
		this.emitter = this.player.emitter;

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
	/** Reconnecting connection whose origin supplies this participant's broadcasts. */
	connection: Moq.Connection;
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

	#connection: Moq.Connection;
	#signals = new Effect();

	constructor(props: RemoteProps) {
		this.identity = props.identity;
		this.#connection = props.connection;
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
