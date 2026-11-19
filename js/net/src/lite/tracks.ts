import { ProtocolViolation } from "../error.ts";
import type * as Instance from "../instance.ts";
import * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { decodeError } from "../util/decode_error.ts";
import { DatagramsInfo } from "./datagram.ts";
import { decodeNode, decodePin, encodeNode, encodePin } from "./instance.ts";
import * as Message from "./message.ts";
import { TrackInfo } from "./track.ts";
import { hasTracks, type Version } from "./version.ts";

const TRACKS_ADD = 0x0;
const TRACKS_REMOVE = 0x1;
const TRACKS_FINISH = 0x2;
// The same event the announce stream marks its initial set with.
const SYNCED = 0x4;

const KIND_TRACK = 0x0;
const KIND_DATAGRAMS = 0x1;

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
	instance?: Instance.Valid;

	constructor(broadcast: Path.Valid, instance?: Instance.Valid) {
		this.broadcast = broadcast;
		this.instance = instance;
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardTracks(version);
		return Message.encode(w, async (w) => {
			await w.string(Path.encode(this.broadcast));
			await encodePin(w, version, this.instance);
			await encodeNode(w, version);
		});
	}

	static async decode(r: Reader, version: Version): Promise<Tracks> {
		guardTracks(version);
		return Message.decode(r, async (r) => {
			const broadcast = Path.decode(await r.string());
			const instance = await decodePin(r, version);
			await decodeNode(r, version);
			return new Tracks(broadcast, instance);
		});
	}
}

/**
 * A listed track's info on a Tracks Stream: the TRACK_INFO a Track Stream would answer, or with
 * `datagrams`, the DATAGRAMS_INFO a Datagrams Stream would.
 */
type Listed = { datagrams?: false; info: TrackInfo } | { datagrams: true; info: DatagramsInfo };

/**
 * One event the publisher sends on a Tracks Stream: TRACKS_ADD with the listed track's kind and
 * info, TRACKS_REMOVE for a track that left unfinished, TRACKS_FINISH for one that finished, or
 * SYNCED once the listing's initial set was sent. A name is listed at most once per kind.
 */
export type TracksEvent =
	| ({ kind: "added"; name: string } & Listed)
	| { kind: "removed"; name: string; datagrams?: boolean }
	| { kind: "finished"; name: string; datagrams?: boolean }
	| { kind: "synced" };

async function encodeKind(w: Writer, datagrams: boolean | undefined): Promise<void> {
	await w.u53(datagrams ? KIND_DATAGRAMS : KIND_TRACK);
}

/** Whether the kind read next is a datagram track; any kind but the two is malformed. */
async function decodeKind(r: Reader): Promise<boolean> {
	const kind = await r.u53();
	if (kind === KIND_TRACK) return false;
	if (kind === KIND_DATAGRAMS) return true;
	throw decodeError(new ProtocolViolation(`unknown listed track kind: ${kind}`));
}

/** A removal's body: its kind, then its name. */
async function decodeRemoval(r: Reader): Promise<{ name: string; datagrams?: boolean }> {
	const datagrams = await decodeKind(r);
	const name = await r.string();
	return datagrams ? { name, datagrams } : { name };
}

/** Write one {@link TracksEvent}: its type, then its size-prefixed body. */
export async function encodeTracksEvent(w: Writer, event: TracksEvent, version: Version): Promise<void> {
	guardTracks(version);
	switch (event.kind) {
		case "added":
			return Message.encode(
				w,
				async (w) => {
					await encodeKind(w, event.datagrams);
					await w.string(event.name);
					await event.info.encode(w, version);
				},
				{ id: TRACKS_ADD },
			);
		case "removed":
		case "finished":
			return Message.encode(
				w,
				async (w) => {
					await encodeKind(w, event.datagrams);
					await w.string(event.name);
				},
				{ id: event.kind === "removed" ? TRACKS_REMOVE : TRACKS_FINISH },
			);
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
			return Message.decode(r, async (r): Promise<TracksEvent> => {
				const datagrams = await decodeKind(r);
				const name = await r.string();
				if (datagrams) return { kind: "added", name, datagrams, info: await DatagramsInfo.decode(r, version) };
				return { kind: "added", name, info: await TrackInfo.decode(r, version) };
			});
		case TRACKS_REMOVE:
			return Message.decode(r, async (r) => ({ kind: "removed", ...(await decodeRemoval(r)) }));
		case TRACKS_FINISH:
			return Message.decode(r, async (r) => ({ kind: "finished", ...(await decodeRemoval(r)) }));
		case SYNCED:
			return Message.decode(r, async () => ({ kind: "synced" }));
		default:
			// A new stream with no earlier builds to tolerate.
			throw decodeError(new ProtocolViolation(`unknown tracks event: ${typ}`));
	}
}
