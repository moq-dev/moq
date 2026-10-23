import type * as Epoch from "../epoch.ts";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { decodeError } from "../util/decode_error.ts";
import { decodeEpoch, encodeEpoch } from "./epoch.ts";
import * as Message from "./message.ts";
import { TrackInfo } from "./track.ts";
import { hasTracks, type Version } from "./version.ts";

const TRACKS_ADD = 0x0;
const TRACKS_REMOVE = 0x1;
const TRACKS_FINISH = 0x2;
// The same event the announce stream marks its initial set with.
const SYNCED = 0x4;

function guardTracks(version: Version) {
	if (!hasTracks(version)) throw new Error("tracks stream not supported for this version");
}

/**
 * TRACKS request: the subscriber's only message on a Tracks Stream (0x8), listing one
 * broadcast's tracks. Lite-07+.
 */
export class Tracks {
	broadcast: Path.Valid;
	/** The publisher instance the subscriber expects. */
	epoch?: Epoch.Valid;

	constructor(broadcast: Path.Valid, epoch?: Epoch.Valid) {
		this.broadcast = broadcast;
		this.epoch = epoch;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTracks(version);
		return Message.encode(w, async (w) => {
			await w.string(Path.encode(this.broadcast));
			await encodeEpoch(w, version, this.epoch);
		});
	}

	static async decode(r: Reader, version: Version): Promise<Tracks> {
		guardTracks(version);
		return Message.decode(r, async (r) => {
			const broadcast = Path.decode(await r.string());
			const epoch = await decodeEpoch(r, version);
			return new Tracks(broadcast, epoch);
		});
	}
}

/**
 * One event the publisher sends on a Tracks Stream: TRACKS_ADD with the TRACK_INFO a Track
 * Stream would answer, TRACKS_REMOVE for a track that left unfinished, TRACKS_FINISH for one that
 * finished, or SYNCED once the listing's initial set was sent.
 */
export type TracksEvent =
	| { kind: "added"; name: string; info: TrackInfo }
	| { kind: "removed"; name: string }
	| { kind: "finished"; name: string }
	| { kind: "synced" };

/** Write one {@link TracksEvent}: its type, then its size-prefixed body. */
export async function encodeTracksEvent(w: Writer, event: TracksEvent, version: Version): Promise<void> {
	guardTracks(version);
	switch (event.kind) {
		case "added":
			return Message.encode(
				w,
				async (w) => {
					await w.string(event.name);
					await event.info.encode(w, version);
				},
				{ id: TRACKS_ADD },
			);
		case "removed":
			return Message.encode(w, (w) => w.string(event.name), { id: TRACKS_REMOVE });
		case "finished":
			return Message.encode(w, (w) => w.string(event.name), { id: TRACKS_FINISH });
		case "synced":
			return Message.encode(w, async () => {}, { id: SYNCED });
	}
}

/** Read the next {@link TracksEvent}, or undefined at the publisher's FIN. */
export async function decodeTracksEventMaybe(r: Reader, version: Version): Promise<TracksEvent | undefined> {
	guardTracks(version);
	if (await r.done()) return undefined;
	const typ = await r.u53();
	switch (typ) {
		case TRACKS_ADD:
			return Message.decode(r, async (r) => {
				const name = await r.string();
				return { kind: "added", name, info: await TrackInfo.decode(r, version) };
			});
		case TRACKS_REMOVE:
			return Message.decode(r, async (r) => ({ kind: "removed", name: await r.string() }));
		case TRACKS_FINISH:
			return Message.decode(r, async (r) => ({ kind: "finished", name: await r.string() }));
		case SYNCED:
			return Message.decode(r, async () => ({ kind: "synced" }));
		default:
			// A new stream with no earlier builds to tolerate.
			throw decodeError(new ProtocolViolation(`unknown tracks event: ${typ}`));
	}
}
