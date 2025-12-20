import type { Path } from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import { type Moq, type Publish, Watch } from "..";

export type Broadcast = Watch.Broadcast | Publish.Broadcast;

export type RoomProps = {
	connection: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;
	path?: Path.Valid | Signal<Path.Valid | undefined>;
};

export class Room {
	// The connection to the server.
	connection: Signal<Moq.Connection.Established | undefined>;

	// An optional prefix to filter broadcasts by.
	path: Signal<Path.Valid | undefined>;

	// The active broadcasts, sorted by announcement time.
	active = new Map<Path.Valid, Broadcast>();

	// All of the remote broadcasts.
	remotes = new Map<Path.Valid, Watch.Broadcast>();

	// The local broadcasts.
	locals = new Map<Path.Valid, Publish.Broadcast>();

	// Optional callbacks to learn when individual broadcasts are added/removed.
	// We avoid using signals because we don't want to re-render everything on every update.
	// One day I'll figure out how to handle collections elegantly.
	#onActive?: (path: Path.Valid, broadcast: Broadcast | undefined) => void;
	#onRemote?: (path: Path.Valid, broadcast: Watch.Broadcast | undefined) => void;
	#onLocal?: (path: Path.Valid, broadcast: Publish.Broadcast | undefined) => void;

	#signals = new Effect();

	constructor(props?: RoomProps) {
		this.connection = Signal.from(props?.connection);
		this.path = Signal.from(props?.path);

		this.#signals.effect(this.#init.bind(this));
	}

	// Render a local broadcast instead of downloading a remote broadcast.
	// This is not a perfect preview, as downloading/decoding is skipped.
	// NOTE: The broadcast is only published when broadcast.enabled is true.
	preview(path: Path.Valid, broadcast: Publish.Broadcast) {
		this.locals.set(path, broadcast);
	}

	unpreview(path: Path.Valid) {
		this.locals.delete(path);
	}

	// Register a callback when a broadcast has been added/removed.
	onActive(callback?: (path: Path.Valid, broadcast: Broadcast | undefined) => void) {
		this.#onActive = callback;
		if (!callback) return;

		for (const [name, broadcast] of this.active) {
			callback(name, broadcast);
		}
	}

	onRemote(callback?: (path: Path.Valid, broadcast: Watch.Broadcast | undefined) => void) {
		this.#onRemote = callback;
		if (!callback) return;

		for (const [name, broadcast] of this.remotes) {
			callback(name, broadcast);
		}
	}

	onLocal(callback?: (path: Path.Valid, broadcast: Publish.Broadcast | undefined) => void) {
		this.#onLocal = callback;
		if (!callback) return;

		for (const [name, broadcast] of this.locals) {
			callback(name, broadcast);
		}
	}

	#init(effect: Effect) {
		const connection = effect.get(this.connection);
		if (!connection) return;

		const url = connection.url;
		if (!url) return;

		const name = effect.get(this.path);

		const announced = connection.announced(name);
		effect.cleanup(() => announced.close());

		effect.spawn(async () => {
			for (;;) {
				const update = await announced.next();
				if (!update) break;

				this.#handleUpdate(update);
			}
		});
	}

	#handleUpdate(update: Moq.AnnouncedEntry) {
		for (const [path, broadcast] of this.locals) {
			if (update.path === path) {
				if (update.active) {
					this.active.set(update.path, broadcast);
					this.#onLocal?.(update.path, broadcast);
					this.#onActive?.(update.path, broadcast);
				} else {
					this.active.delete(update.path);
					this.#onLocal?.(update.path, undefined);
					this.#onActive?.(update.path, undefined);
				}
				return;
			}
		}

		if (update.active) {
			// NOTE: If you were implementing this yourself, you could use the <hang-watch> element instead.
			const watch = new Watch.Broadcast({
				connection: this.connection,
				// NOTE: You're responsible for setting enabled to true if you want to download the broadcast.
				enabled: false,
				path: update.path,
				reload: false,
			});

			this.remotes.set(update.path, watch);
			this.active.set(update.path, watch);

			this.#onRemote?.(update.path, watch);
			this.#onActive?.(update.path, watch);
		} else {
			const existing = this.remotes.get(update.path);
			if (!existing) throw new Error(`broadcast not found: ${update.path}`);

			existing.close();
			this.remotes.delete(update.path);
			this.active.delete(update.path);

			this.#onRemote?.(update.path, undefined);
			this.#onActive?.(update.path, undefined);
		}
	}

	close() {
		this.#signals.close();

		// Swap out the maps so they're empty when the callbacks run.
		const remotes = this.remotes;
		const active = this.active;
		const locals = this.locals;

		this.remotes = new Map();
		this.active = new Map();
		this.locals = new Map();

		// Clear all remote/active broadcasts when there are no more announcements.
		for (const [name, broadcast] of remotes) {
			broadcast.close();
			this.#onRemote?.(name, undefined);
		}

		for (const name of locals.keys()) {
			this.#onLocal?.(name, undefined);
		}

		for (const name of active.keys()) {
			this.#onActive?.(name, undefined);
		}
	}
}
