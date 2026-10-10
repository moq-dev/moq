import { Session, SessionCode } from "../error.ts";

/** The default maximum number of live advertisements on one session. @internal */
export const ANNOUNCES = 100_000;
/** The default maximum number of live subscriptions on one session. @internal */
export const SUBSCRIPTIONS = 10_000;

/** A live-resource budget that closes its session when the peer exceeds it. @internal */
export class Slots {
	#count = 0;
	#limit: number;
	#quic?: WebTransport;

	constructor(limit: number, quic?: WebTransport) {
		this.#limit = limit;
		this.#quic = quic;
	}

	acquire() {
		if (this.#count === this.#limit) {
			const error = new Session(SessionCode.TooManyRequests, { reason: "too many live requests" });
			this.#quic?.close({ closeCode: error.code, reason: error.message });
			throw error;
		}
		this.#count += 1;
	}

	release(count = 1) {
		this.#count -= count;
	}
}

type Kind = "track" | "subscribe";
type Entry = { track: number; subscribe: number };

/** Charges paired TRACK and SUBSCRIBE interests once, matching moq-net. @internal */
export class Interests {
	#slots: Slots;
	#paths = new Map<string, Map<string, Entry>>();

	constructor(slots: Slots) {
		this.#slots = slots;
	}

	acquire(path: string, track: string, kind: Kind) {
		let tracks = this.#paths.get(path);
		const entry = tracks?.get(track) ?? { track: 0, subscribe: 0 };
		// For each name the cost is max(TRACKs, SUBSCRIBEs), not their sum.
		if (entry[kind] >= entry[kind === "track" ? "subscribe" : "track"]) this.#slots.acquire();
		entry[kind] += 1;
		if (!tracks) {
			tracks = new Map();
			this.#paths.set(path, tracks);
		}
		tracks.set(track, entry);
		return new Interest({ interests: this, path, track, kind });
	}

	release(path: string, track: string, kind: Kind) {
		const tracks = this.#paths.get(path);
		const entry = tracks?.get(track);
		if (!tracks || !entry) throw new Error("interest already released");
		const before = Math.max(entry.track, entry.subscribe);
		entry[kind] -= 1;
		if (Math.max(entry.track, entry.subscribe) < before) this.#slots.release();
		if (entry.track === 0 && entry.subscribe === 0) {
			tracks.delete(track);
			if (tracks.size === 0) this.#paths.delete(path);
		}
	}
}

/** One held interest, released at most once. @internal */
class Interest {
	#interests?: Interests;
	#path: string;
	#track: string;
	#kind: Kind;

	constructor({ interests, path, track, kind }: { interests: Interests; path: string; track: string; kind: Kind }) {
		this.#interests = interests;
		this.#path = path;
		this.#track = track;
		this.#kind = kind;
	}

	release() {
		this.#interests?.release(this.#path, this.#track, this.#kind);
		this.#interests = undefined;
	}
}
