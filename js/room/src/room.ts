/**
 * A room is a path prefix. Participants are discovered from the announce
 * stream; identity is the path before `camera`/`screen`.
 *
 * @module
 */

import * as Moq from "@moq/net";
import { Effect, type Getter, type GetterInit, getter, Signal } from "@moq/signals";
import { type Kind, parse } from "./path.ts";
import { Remote } from "./remote.ts";

/** Constructor options for {@link Room}. */
export interface RoomProps {
	/**
	 * Reconnecting connection whose URL (and token root) already carry the room
	 * prefix. Announcements are relative to that prefix.
	 */
	connection: Moq.Connection;
	/**
	 * Local participant identity. Announcements under this identity are skipped
	 * so the local camera/screen do not appear as remotes.
	 */
	identity?: GetterInit<Moq.Path.Valid | undefined>;
	/** When false, the announce loop is idle. Defaults to true. */
	enabled?: GetterInit<boolean>;
	/**
	 * Announce prefix relative to the connection URL. Defaults to empty (the
	 * whole root). A connection whose URL is broader than one room (a preview
	 * of several rooms) passes the room name here.
	 */
	prefix?: GetterInit<Moq.Path.Valid | undefined>;
}

/**
 * Runs the announce loop and exposes remote participants as a signal map keyed
 * by identity.
 */
export class Room {
	/** Connection supplying room announcements. */
	readonly connection: Moq.Connection;
	/** Local identity excluded from the roster. */
	readonly identity: Getter<Moq.Path.Valid | undefined>;
	/** Whether room discovery is active. */
	readonly enabled: Getter<boolean>;
	/** Room prefix relative to the connection root. */
	readonly prefix: Getter<Moq.Path.Valid | undefined>;

	#remotes = new Signal(new Map<Moq.Path.Valid, Remote>());
	#signals = new Effect();

	constructor(props: RoomProps) {
		this.connection = props.connection;
		this.identity = getter(props.identity);
		this.enabled = getter(props.enabled ?? true);
		this.prefix = getter(props.prefix);

		this.#signals.run((effect) => {
			if (!effect.get(this.enabled)) return;

			effect.get(this.identity);
			const origin = effect.get(this.connection.origin);
			if (!origin) return;
			const prefix = effect.get(this.prefix) ?? Moq.Path.empty();
			const announced = origin.announced(Moq.Path.Pattern.subtree(prefix));
			effect.cleanup(() => announced.close());

			effect.spawn(this.#run.bind(this, announced, prefix, effect));
			effect.cleanup(() => {
				for (const remote of this.#remotes.peek().values()) remote.close();
				this.#remotes.set(new Map());
			});
		});
	}

	/** Remote participants, keyed by identity. */
	get remotes(): Getter<Map<Moq.Path.Valid, Remote>> {
		return this.#remotes;
	}

	async #run(announced: Moq.Announce.Consumer, prefix: Moq.Path.Valid, effect: Effect): Promise<void> {
		for (;;) {
			const update = await Promise.race([effect.cancel, announced.next()]);
			if (!update) break;

			// The scope's `**` captures what lies beneath the prefix. A broad route
			// that cannot pin that suffix names no participant to open.
			const capture = update.captures?.[0];
			const suffix = capture?.isLiteral ? capture.text : capture?.asPrefix();
			if (suffix === undefined) continue;
			const parsed = parse(Moq.Path.from(suffix));
			if (!parsed) continue;
			const covered = Moq.Path.join(prefix, Moq.Path.from(suffix));

			const local = this.identity.peek();
			if (local && parsed.identity === local) continue;

			if (Moq.Announce.isActive(update.kind)) {
				this.#add(parsed.identity, parsed.kind, Moq.Path.from(covered));
			} else {
				this.#remove(parsed.identity, parsed.kind);
			}
		}
	}

	#add(identity: Moq.Path.Valid, kind: Kind, path: Moq.Path.Valid): void {
		let remote = this.#remotes.peek().get(identity);
		if (!remote) {
			const created = new Remote({ identity, connection: this.connection });
			this.#remotes.mutate((remotes) => remotes.set(identity, created));
			remote = created;
		}
		remote.attach(kind, path);
	}

	#remove(identity: Moq.Path.Valid, kind: Kind): void {
		const remote = this.#remotes.peek().get(identity);
		if (!remote) return;
		remote.detach(kind);
		if (remote.empty()) {
			remote.close();
			this.#remotes.mutate((remotes) => remotes.delete(identity));
		}
	}

	/** Release this participant's subscriptions and media resources. */
	close() {
		this.#signals.close();
	}
}
