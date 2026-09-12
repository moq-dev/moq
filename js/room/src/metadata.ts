/**
 * The `hang/*.json` metadata convention: a catalog section pointing at JSON
 * snapshot tracks on the same broadcast.
 *
 * Core carries `user.json` (id, name, avatar) and `preview.json` (presence
 * booleans). Chat and location ride the same catalog section as app-defined
 * extensions; this module does not serve or consume them.
 *
 * @module
 */

import type { Root as CatalogRoot } from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type * as Publish from "@moq/publish";
import { Effect, type Getter, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Watch from "@moq/watch";

/** Delivery priority for hang metadata tracks: below the catalog, above audio. */
export const PRIORITY = 90;

/**
 * Well-known hang catalog tracks.
 *
 * `user` and `preview` are served by this package. `chat` and `location` are
 * app-defined extensions of the same catalog section (hang.live uses both).
 */
export const TRACK = {
	user: "hang/user.json",
	preview: "hang/preview.json",
	chat: "hang/chat.json",
	location: "hang/location.json",
} as const;

/** Display name, id, and avatar published on `hang/user.json`. */
export type User = {
	id?: string;
	name?: string;
	avatar?: string;
	color?: string;
};

/** Presence published on `hang/preview.json`. */
export type Preview = {
	audio?: boolean;
	video?: boolean;
	screen?: boolean;
	name?: string;
	avatar?: string;
	chat?: boolean;
	typing?: boolean;
};

type TrackRef = {
	track: string;
};

/** The `hang` catalog section. Extra keys (chat, location) pass through. */
export type HangCatalog = {
	user?: TrackRef;
	preview?: TrackRef;
	chat?: TrackRef;
	location?: TrackRef;
};

/** A hang catalog with the optional `hang` section. */
export type ExtendedCatalog = CatalogRoot & {
	hang?: HangCatalog;
};

/** Signals a publisher reads when serving user metadata. */
export type UserInput = {
	id: Getter<string | undefined>;
	name: Getter<string | undefined>;
	avatar: Getter<string | undefined>;
	color: Getter<string | undefined>;
};

/** Writable user fields a publisher owns. */
export type UserFields = {
	id: Signal<string | undefined>;
	name: Signal<string | undefined>;
	avatar: Signal<string | undefined>;
	color: Signal<string | undefined>;
};

/** Seed values for {@link UserFields}. */
export type UserProps = {
	id?: string | Signal<string | undefined>;
	name?: string | Signal<string | undefined>;
	avatar?: string | Signal<string | undefined>;
	color?: string | Signal<string | undefined>;
};

/** Create the writable user signals a publisher owns. */
export function userFields(props?: UserProps): UserFields {
	return {
		id: Signal.from(props?.id),
		name: Signal.from(props?.name),
		avatar: Signal.from(props?.avatar),
		color: Signal.from(props?.color),
	};
}

/**
 * Publish `user.json` and `preview.json` on `broadcast`, and advertise them in
 * the catalog's `hang` section. Extra hang keys already on the catalog are left
 * alone so an app can add chat/location without fighting this.
 */
export function serve(broadcast: Publish.Broadcast, user: UserInput, preview: Getter<Preview>, effect: Effect): void {
	broadcast.catalog.mutate((catalog) => {
		const extended = catalog as ExtendedCatalog;
		if (!extended.hang) extended.hang = {};
		extended.hang.user = { track: TRACK.user };
		extended.hang.preview = { track: TRACK.preview };
	});

	effect.cleanup(() => {
		broadcast.catalog.mutate((catalog) => {
			const hang = (catalog as ExtendedCatalog).hang;
			if (!hang) return;
			delete hang.user;
			delete hang.preview;
			if (!hang.chat && !hang.location) {
				delete (catalog as ExtendedCatalog).hang;
			}
		});
	});

	serveSnapshot(broadcast, TRACK.user, effect, (effect) => ({
		id: effect.get(user.id),
		name: effect.get(user.name),
		avatar: effect.get(user.avatar),
		color: effect.get(user.color),
	}));

	serveSnapshot(broadcast, TRACK.preview, effect, (effect) => ({
		info: effect.get(preview),
	}));
}

function serveSnapshot<T>(
	broadcast: Publish.Broadcast,
	name: string,
	effect: Effect,
	value: (effect: Effect) => T,
): void {
	effect.run((effect) => {
		const net = effect.get(broadcast.net);
		if (!net) return;

		// A day-long cache so a late joiner still replays the latest value.
		const track = net.createTrack(name, { latencyMax: 86_400_000, priority: PRIORITY });
		effect.cleanup(() => track.close());

		const producer = new Json.Snapshot.Producer<T>({ track, initial: value(effect) });
		effect.cleanup(() => producer.finish());

		effect.run((effect) => {
			producer.update(value(effect));
		});
	});
}

type Consumed = {
	user: Readonlys<UserInput>;
	preview: Getter<Preview>;
	close: () => void;
};

/**
 * Subscribe to `user.json` and `preview.json` on a watched broadcast.
 *
 * Track names come from the catalog's `hang` section so a publisher that
 * renamed them still works. Missing sections leave the signals empty.
 */
export function consume(broadcast: Watch.Broadcast): Consumed {
	const user = {
		id: new Signal<string | undefined>(undefined),
		name: new Signal<string | undefined>(undefined),
		avatar: new Signal<string | undefined>(undefined),
		color: new Signal<string | undefined>(undefined),
	};
	const preview = new Signal<Preview>({});
	const signals = new Effect();

	signals.run((effect) => {
		const catalog = effect.get(broadcast.out.catalog) as ExtendedCatalog | undefined;
		const hang = catalog?.hang;
		const active = effect.get(broadcast.out.active);
		if (!active || !hang) return;

		if (hang.user) {
			subscribeJson<User>(active, hang.user.track, effect, (value) => {
				user.id.set(value.id);
				user.name.set(value.name);
				user.avatar.set(value.avatar);
				user.color.set(value.color);
			});
		}

		if (hang.preview) {
			subscribeJson<{ info?: Preview }>(active, hang.preview.track, effect, (value) => {
				preview.set(value.info ?? {});
			});
		}
	});

	return {
		user: readonlys(user),
		preview,
		close: () => signals.close(),
	};
}

function subscribeJson<T>(
	broadcast: Moq.Broadcast.Consumer,
	name: string,
	effect: Effect,
	update: (value: T) => void,
): void {
	const track = broadcast.track(name).subscribe({ priority: PRIORITY });
	effect.cleanup(() => track.close());

	const consumer = new Json.Snapshot.Consumer<T>(track);
	effect.spawn(async () => {
		for (;;) {
			const value = await Promise.race([effect.cancel, consumer.next()]);
			if (value === undefined) break;
			update(value);
		}
	});
}
