import { race, Signal } from "@moq/signals";
import { error, NotFound, ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import { type FillGroups, fillChanged, wantedRuns } from "../fill.ts";
import type * as group from "../group.ts";
import { hooks } from "../internal.ts";
import { receiveFrame, skipPayload } from "../payload.ts";
import type { Cursor, Reader } from "../stream.ts";
import type { Timescale } from "../time.ts";
import type * as track from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { isDraft20 } from "./filter.ts";
import { decodeFetchObject, EndOfRange, hasGroupEnd, objectDeltaBias, objectTime } from "./object.ts";
import type { IetfVersion } from "./version.ts";

// The first Object ID past the safe range: no fetched object can carry it, so an End of Range
// reaching it leaves nothing to follow in its group.
const OBJECT_LIMIT = 1n << 53n;

/** The groups one standalone FETCH writes, sharing the joining FETCH's object and payload decoder. @internal */
export class RangeFetch {
	readonly fill: FillGroups;
	readonly first: bigint;
	readonly last: bigint;
	readonly bound?: number;
	readonly #track: track.Producer;
	readonly #timescale?: Timescale;
	readonly #timestampIds: boolean;
	readonly #version: IetfVersion;
	readonly #changed = new Signal(0);
	readonly #stop = new AbortController();
	readonly #done = Promise.withResolvers<void>();
	#next: bigint;
	#current?: { key: bigint; producer?: group.Producer };
	#taken = false;
	#finished = false;

	constructor(options: {
		fill: FillGroups;
		first: bigint;
		track: track.Producer;
		timescale?: Timescale;
		timestampIds?: boolean;
		last: bigint;
		bound?: number;
		version: IetfVersion;
	}) {
		this.fill = options.fill;
		this.first = this.#next = options.first;
		this.last = options.last;
		this.bound = options.bound;
		this.#track = options.track;
		this.#timescale = options.timescale;
		this.#timestampIds = options.timestampIds ?? false;
		this.#version = options.version;
	}

	/** Settles when the stream finishes or the request cancels it. */
	get done(): Promise<void> {
		return this.#done.promise;
	}

	/** Stop once no reader wants the remaining groups or the unfinished group already handed out. */
	async unwanted(): Promise<void> {
		for (;;) {
			const demand = this.#current?.producer?.demand();
			if (
				this.#finished ||
				(!wantedRuns(this.fill).some((run) => run.start <= this.last && run.end > this.first) &&
					!demand?.wanted.peek())
			)
				return;
			await race([fillChanged(this.fill), Signal.race(this.#changed, ...(demand ? [demand.wanted] : []))]);
		}
	}

	/** Abort only this stream's unfinished group and refuse what it still owes. Complete history stays cached. */
	cancel(err: Error): void {
		if (this.#finished) return;
		this.#current?.producer?.abort(err);
		this.fill.refuseGroups(this.#next, this.last + 1n, err);
		this.#finish(err);
	}

	#finish(err?: Error): void {
		this.#finished = true;
		this.#stop.abort(err ?? new StreamError(StreamCode.Cancel, { message: "fetch complete" }));
		this.#changed.update((n) => n + 1);
		this.#done.resolve();
	}

	#closeGroup(): void {
		this.#current?.producer?.finish();
		this.#current = undefined;
		this.#changed.update((n) => n + 1);
	}

	#open(key: bigint): void {
		if (key < this.#next || key > this.last)
			throw new ProtocolViolation("FETCH group is out of order or past FETCH_OK's End Location");
		this.#closeGroup();
		this.fill.dropGroups(this.#next, key);
		// A live stream may have answered this key while the FETCH was in flight.
		const producer = this.fill.isWanted(key) ? this.fill.accept(key) : undefined;
		this.#current = { key: key, producer };
		this.#next = key + 1n;
		this.#changed.update((n) => n + 1);
	}

	/** Read one fetch stream after its header, bounded by FETCH_OK, accepting each group at its first object. */
	async receive(stream: Reader): Promise<void> {
		if (this.#taken || this.#finished) {
			stream.stop(new StreamError(StreamCode.Cancel, { message: "FETCH stream no longer wanted" }));
			return;
		}
		this.#taken = true;
		const version = this.#version;
		let prior: bigint | undefined;
		let next = 0;
		let first = true;
		let trailing = false;
		// The prior entry was draft-23's End of Group, so the group takes no more objects.
		let ended = false;
		try {
			const decode = (c: Cursor) => decodeFetchObject(c);
			for (;;) {
				const object = await untilAborted(
					Promise.resolve(stream.tryDecode(decode) ?? stream.decodeMaybe(decode)),
					this.#stop.signal,
				);
				if (object === undefined) break;
				if (object.kind === "endOfGroup") {
					if (prior === undefined) throw new ProtocolViolation("a fetch stream starts with an End of Group");
					// Draft-23 says where the track ends here rather than in FETCH_OK.
					if (object.track) hooks.endTrack(this.#track, { group: prior + 1n, frame: 0 });
					// The group is whole now, so a stream that stalls or resets after this keeps it.
					this.#closeGroup();
					ended = true;
					continue;
				}
				if (object.kind === "endOfRange") {
					// Draft-23 encodes every End of Range but the stream's first entry against the
					// prior entry (section 11.4.2.1); within its group the Object ID steps past it.
					const delta = hasGroupEnd(version) && prior !== undefined;
					const group = delta && prior !== undefined ? prior + object.group : object.group;
					const at = delta && object.group === 0n ? BigInt(next) + object.object : object.object;
					if (ended && delta && object.group === 0n)
						throw new ProtocolViolation("an End of Range continues a group past its End of Group");
					ended = false;
					if (
						group < (prior ?? this.first) ||
						group > this.last ||
						(this.bound !== undefined && group === this.last && at >= BigInt(this.bound))
					)
						throw new ProtocolViolation("End of Range is outside FETCH_OK's range");
					if (this.#current?.key === group) {
						if (object.reason !== EndOfRange.NonExistent)
							throw new NotFound("FETCH group suffix unavailable");
						trailing = true;
					} else {
						this.#closeGroup();
						if (object.reason === EndOfRange.NonExistent) this.fill.dropGroups(this.#next, group);
						else this.fill.refuseGroups(this.#next, group, new NotFound("FETCH range unavailable"));
						this.fill.refuseGroups(group, group + 1n, new NotFound("FETCH omitted a group prefix"));
						this.#current = { key: group };
						this.#next = group + 1n;
					}
					prior = group;
					next = Number(at < OBJECT_LIMIT ? at + 1n : OBJECT_LIMIT);
					first = true;
					continue;
				}
				// Past an End of Group only an object that names another group can follow.
				if (ended && object.group === undefined)
					throw new ProtocolViolation("a fetch object continues a group past its End of Group");
				ended = false;
				if (
					first &&
					(object.group === undefined ||
						object.object === undefined ||
						object.priority === undefined ||
						object.subgroup === "prior" ||
						object.subgroup === "next")
				)
					throw new ProtocolViolation("the first fetch object refers to a prior object");
				if (
					object.subgroup === "datagram" ||
					object.subgroup === "next" ||
					(object.subgroup === "explicit" && object.subgroupId !== 0n)
				)
					throw new Error("FETCH subgroups and datagrams are unsupported");
				const key =
					object.group === undefined ? prior : prior === undefined ? object.group : prior + object.group + 1n;
				if (key === undefined || key > 0xffff_ffff_ffff_ffffn)
					throw new ProtocolViolation("invalid FETCH Group ID");
				if (this.#current?.key !== key) {
					this.#open(key);
					next = 0;
					trailing = false;
				}
				if (trailing) throw new Error("FETCH object gap is unsupported");
				const id =
					object.group === undefined
						? object.object === undefined
							? next
							: next - 1 + object.object + objectDeltaBias(version)
						: object.object;
				if (id === undefined || !Number.isSafeInteger(id) || id < next)
					throw new ProtocolViolation("FETCH object ID decreased");
				if (!this.#timestampIds && id !== next) throw new Error("FETCH object gap is unsupported");
				if (key === this.last && this.bound !== undefined && id >= this.bound)
					throw new ProtocolViolation("FETCH continued past FETCH_OK's End Location");
				const producer = this.#current?.producer;
				if (producer) {
					const timestamp = this.#timestampIds
						? objectTime(this.#timescale, producer.start, BigInt(id), object.tie ?? 0n)
						: objectTime(this.#timescale, producer.start);
					if (object.payload) producer.writeFrame({ payload: object.payload, timestamp });
					else if (
						!(await untilAborted(
							receiveFrame(stream, producer, { size: object.size, timestamp }),
							this.#stop.signal,
						))
					)
						throw new Error("FETCH group was abandoned");
				} else if (!object.payload) await untilAborted(skipPayload(stream, object.size), this.#stop.signal);
				prior = key;
				next = id + 1;
				first = false;
			}
			if (
				!isDraft20(version) &&
				this.bound !== undefined &&
				(this.#current?.key !== this.last || next < this.bound)
			)
				throw new ProtocolViolation("FETCH ended before FETCH_OK's End Location");
			this.#closeGroup();
			this.fill.dropGroups(this.#next, this.last + 1n);
			this.#finish();
		} catch (cause) {
			const err = error(cause);
			this.cancel(err);
			if (err instanceof StreamError && err.code === StreamCode.MalformedTrack) this.#track.abort(err);
			stream.stop(err);
			throw err;
		}
	}
}
