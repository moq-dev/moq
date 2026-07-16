const TRACK_ALIAS_TIMEOUT_MS = 1000;

type Resolver<T> = PromiseWithResolvers<T>["resolve"];

/** Resolves publisher-chosen track aliases after control/data stream reordering. @internal */
export class TrackAliases<T> {
	#active = new Map<bigint, T>();
	#pending = new Map<bigint, Set<Resolver<T>>>();

	/** Waits briefly for an alias to be established by SUBSCRIBE_OK or PUBLISH. */
	async get(alias: bigint): Promise<T> {
		if (this.#active.has(alias)) return this.#active.get(alias) as T;

		const { promise, resolve } = Promise.withResolvers<T>();
		let resolvers = this.#pending.get(alias);
		if (!resolvers) {
			resolvers = new Set();
			this.#pending.set(alias, resolvers);
		}
		resolvers.add(resolve);

		let timer: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timer = setTimeout(() => reject(new Error(`unknown track alias: ${alias}`)), TRACK_ALIAS_TIMEOUT_MS);
		});

		try {
			return await Promise.race([promise, timeout]);
		} finally {
			clearTimeout(timer);
			resolvers.delete(resolve);
			if (this.#pending.get(alias) === resolvers && resolvers.size === 0) this.#pending.delete(alias);
		}
	}

	/** Establishes an alias and releases any data streams waiting for it. */
	set(alias: bigint, value: T) {
		const active = this.#active.get(alias);
		if (this.#active.has(alias)) {
			if (active !== value) throw new Error(`duplicate track alias: ${alias}`);
			return;
		}

		this.#active.set(alias, value);
		const resolvers = this.#pending.get(alias);
		this.#pending.delete(alias);
		for (const resolve of resolvers ?? []) resolve(value);
	}

	/** Removes an alias only if it still belongs to the supplied value. */
	delete(alias: bigint, value: T) {
		if (this.#active.get(alias) === value) this.#active.delete(alias);
	}
}
