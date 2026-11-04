/**
 * The `hang/*.json` metadata convention: a catalog section pointing at JSON
 * snapshot tracks on the same broadcast.
 *
 * Core carries `user.json` (id, name, avatar) and `preview.json` (presence
 * booleans). Location (and hang.live's JSON chat) ride the same catalog
 * section as app-defined extensions; this module does not serve or consume
 * them. The JSON window `chat` track is `Chat.TRACK` (`"chat"`), not
 * `hang/chat.json`.
 *
 * @module
 */

import { Milli } from "@moq/clock";
import type { Root as CatalogRoot } from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import type * as Publish from "@moq/publish";
import { Effect, type Getter, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Watch from "@moq/watch";
import * as z from "zod/mini";

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

// A remote publisher's values are untrusted, so a consumer validates each against these. Keep them
// in step with the types above, which stay spelled out for the API docs.
const UserSchema: z.ZodMiniType<User> = z.object({
	id: z.optional(z.string()),
	name: z.optional(z.string()),
	avatar: z.optional(z.string()),
	color: z.optional(z.string()),
});

const PreviewSchema: z.ZodMiniType<Preview> = z.object({
	audio: z.optional(z.boolean()),
	video: z.optional(z.boolean()),
	screen: z.optional(z.boolean()),
	name: z.optional(z.string()),
	avatar: z.optional(z.string()),
	chat: z.optional(z.boolean()),
	typing: z.optional(z.boolean()),
});

// `hang/preview.json` wraps the presence in `info`.
const PreviewTrackSchema = z.object({ info: z.optional(PreviewSchema) });

/** Points a `hang` catalog section at a track. */
export type TrackRef = {
	/** The track name. */
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

		// A day-long cache so a late joiner still replays the latest value. Each value is stamped
		// when written, so the track is timed.
		const track = net.createTrack(name, {
			timescale: Moq.Time.Timescale.MILLI,
			maxAge: Milli(86_400_000),
			priority: PRIORITY,
		});
		effect.cleanup(() => {
			track.finish();
			track.close();
		});

		const producer = new Json.Snapshot.Producer<T>({ track });
		effect.cleanup(() => producer.finish());

		effect.run((effect) => {
			producer.update({ value: value(effect), at: Moq.Time.Timestamp.now() });
		});
	});
}

/** The signals {@link consume} reads, and how to stop reading them. */
export type Consumed = {
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
		effect.cleanup(() => {
			for (const field of Object.values(user)) field.set(undefined);
			preview.set({});
		});
		if (!active || !hang) return;

		if (hang.user) {
			subscribeJson(active, hang.user.track, UserSchema, effect, (value) => {
				user.id.set(value.id);
				user.name.set(value.name);
				user.avatar.set(value.avatar);
				user.color.set(value.color);
			});
		}

		if (hang.preview) {
			subscribeJson(active, hang.preview.track, PreviewTrackSchema, effect, (value) => {
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

/**
 * Feed each value on `name` that matches `schema` to `update`.
 *
 * A value that does not match is refused with a warning and the last valid one stays, so a
 * malformed publish neither reaches the UI nor ends the subscription.
 */
function subscribeJson<T>(
	broadcast: Moq.Broadcast.Consumer,
	name: string,
	schema: z.ZodMiniType<T>,
	effect: Effect,
	update: (value: T) => void,
): void {
	const track = broadcast.track(name).subscribe({ ...Moq.Track.Subscription.default(), priority: PRIORITY });
	effect.cleanup(() => track.close());

	const consumer = new Json.Snapshot.Consumer<T>({ track, schema });
	effect.spawn(async () => {
		for (;;) {
			let state: Moq.Timed<T> | undefined;
			try {
				state = await effect.race(consumer.latest());
			} catch (err) {
				// The value was decoded and only failed the schema, so the next frame still applies.
				if (!(err instanceof z.core.$ZodError)) throw err;
				console.warn(`refused invalid ${name}:`, err);
				continue;
			}
			if (state === undefined) break;
			update(state.value);
		}
	});
}
