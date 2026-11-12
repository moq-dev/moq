/**
 * Minting instances, which only a session does. Not exported from the package entrypoint, and
 * {@link Minted}'s constructor refuses anyone without this module's token, so the public API
 * builds only named instances.
 *
 * @module
 */

const TOKEN = Symbol("mint");

// Set by Minted's static block, which alone may call its private constructor.
let construct: (text: string) => Minted;

/**
 * An instance a session minted for a route whose wire (an older moq-lite, or moq-transport)
 * carries none. Interned by text while any copy is alive, so `===` and map keys compare minted
 * instances by value, and a named instance, being a string, never equals one.
 */
export class Minted {
	readonly #text: string;

	// The token refuses a caller that reaches the class through an instance's prototype.
	private constructor(token: symbol, text: string) {
		if (token !== TOKEN) throw new TypeError("only a session mints an instance");
		this.#text = text;
		Object.freeze(this);
	}

	static {
		construct = (text) => new Minted(TOKEN, text);
	}

	/** Whether `value` is a minted instance: a brand check no other object passes. */
	static is(value: unknown): value is Minted {
		return typeof value === "object" && value !== null && #text in value;
	}

	/** The instance's text, which alone does not tell it from a named instance. */
	get text(): string {
		return this.#text;
	}

	toString(): string {
		return this.#text;
	}
}

// One live Minted per text, so equality stays `===`. An entry goes once nothing holds its
// instance, after which nobody can compare against it.
const interned = new Map<string, WeakRef<Minted>>();
const released = new FinalizationRegistry<string>((text) => {
	if (interned.get(text)?.deref() === undefined) interned.delete(text);
});

/** The minted instance with `text`. */
export function mint(text: string): Minted {
	const live = interned.get(text)?.deref();
	if (live) return live;
	const minted = construct(text);
	interned.set(text, new WeakRef(minted));
	released.register(minted, text);
	return minted;
}

/** 128 random bits as hex. */
export function hex(): string {
	const bytes = crypto.getRandomValues(new Uint8Array(16));
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** A fresh instance a session mints for a route its wire delivered without one. */
export function fresh(): Minted {
	return mint(hex());
}
