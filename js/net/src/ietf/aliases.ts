const TRACK_ALIAS_TIMEOUT_MS = 1000;

interface Pending<T> extends PromiseWithResolvers<T> {
	waiters: number;
}

/** Resolves publisher-chosen track aliases after control/data stream reordering. @internal */
export class TrackAliases<T> {
	#active = new Map<bigint, T>();
	#pending = new Map<bigint, Pending<T>>();

	/** Waits briefly for an alias to be established by SUBSCRIBE_OK or PUBLISH. */
	async get(alias: bigint): Promise<T> {
		if (this.#active.has(alias)) return this.#active.get(alias) as T;

		let pending = this.#pending.get(alias);
		if (!pending) {
			pending = { ...Promise.withResolvers<T>(), waiters: 0 };
			this.#pending.set(alias, pending);
		}
		pending.waiters += 1;

		let timer: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timer = setTimeout(() => reject(new Error(`unknown track alias: ${alias}`)), TRACK_ALIAS_TIMEOUT_MS);
		});

		try {
			return await Promise.race([pending.promise, timeout]);
		} finally {
			clearTimeout(timer);
			pending.waiters -= 1;
			if (this.#pending.get(alias) === pending && pending.waiters === 0) this.#pending.delete(alias);
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
		const pending = this.#pending.get(alias);
		this.#pending.delete(alias);
		pending?.resolve(value);
	}

	/** Removes an alias only if it still belongs to the supplied value. */
	delete(alias: bigint, value: T) {
		if (this.#active.get(alias) === value) this.#active.delete(alias);
	}
}
