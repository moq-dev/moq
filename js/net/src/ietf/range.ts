import { race, Signal } from "@moq/signals";
import { error, NotFound, ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import { type Fill, fillChanged, wantedRuns } from "../fill.ts";
import type * as group from "../group.ts";
import { receiveFrame, skipPayload } from "../payload.ts";
import type { Cursor, Reader } from "../stream.ts";
import type { Timescale } from "../time.ts";
import type * as track from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { decodeFetchObject, EndOfRange } from "./object.ts";

/** The groups one standalone FETCH writes, sharing the joining FETCH's object and payload decoder. @internal */
export class RangeFetch {
	readonly fill: Fill;
	readonly first: bigint;
	readonly last: bigint;
	readonly bound?: number;
	readonly #track: track.Producer;
	readonly #timescale?: Timescale;
	readonly #legacy: boolean;
	readonly #changed = new Signal(0);
	readonly #stop = new AbortController();
	readonly #done = Promise.withResolvers<void>();
	#next: bigint;
	#current?: { sequence: bigint; producer?: group.Producer };
	#taken = false;
	#finished = false;

	constructor(options: {
		fill: Fill;
		first: bigint;
		track: track.Producer;
		timescale?: Timescale;
		last: bigint;
		bound?: number;
		legacy: boolean;
	}) {
		this.fill = options.fill;
		this.first = this.#next = options.first;
		this.last = options.last;
		this.bound = options.bound;
		this.#track = options.track;
		this.#timescale = options.timescale;
		this.#legacy = options.legacy;
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

	#open(sequence: bigint): void {
		if (sequence < this.#next || sequence > this.last)
			throw new ProtocolViolation("FETCH group is out of order or past FETCH_OK's End Location");
		this.#closeGroup();
		this.fill.dropGroups(this.#next, sequence);
		// A live stream may have answered this sequence while the FETCH was in flight.
		const producer = this.fill.isWanted(sequence) ? this.fill.accept(sequence) : undefined;
		this.#current = { sequence, producer };
		this.#next = sequence + 1n;
		this.#changed.update((n) => n + 1);
	}

	/** Read one fetch stream after its header, bounded by FETCH_OK, accepting each group at its first object. */
	async receive(stream: Reader): Promise<void> {
		if (this.#taken || this.#finished) throw new Error("FETCH stream no longer wanted");
		this.#taken = true;
		let prior: bigint | undefined;
		let next = 0;
		let first = true;
		try {
			const decode = (c: Cursor) => decodeFetchObject(c, this.#timescale);
			for (;;) {
				const object = await untilAborted(
					Promise.resolve(stream.tryDecode(decode) ?? stream.decodeMaybe(decode)),
					this.#stop.signal,
				);
				if (object === undefined) break;
				if (object.kind === "endOfRange") {
					if (
						object.group < (prior ?? this.first) ||
						object.group > this.last ||
						(this.bound !== undefined && object.group === this.last && object.object >= this.bound)
					)
						throw new ProtocolViolation("End of Range is outside FETCH_OK's range");
					if (this.#current?.sequence === object.group) {
						// The model cannot carry holes between frame ordinals. Refuse this group whole.
						this.#current.producer?.abort(new NotFound("FETCH omitted objects within a group"));
						this.#current.producer = undefined;
					} else {
						this.#closeGroup();
						if (object.reason === EndOfRange.NonExistent) this.fill.dropGroups(this.#next, object.group);
						else this.fill.refuseGroups(this.#next, object.group, new NotFound("FETCH range unavailable"));
						this.fill.refuseGroups(
							object.group,
							object.group + 1n,
							new NotFound("FETCH omitted a group prefix"),
						);
						this.#current = { sequence: object.group };
						this.#next = object.group + 1n;
					}
					prior = object.group;
					next = object.object + 1;
					first = true;
					continue;
				}
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
					(object.subgroup === "explicit" && object.subgroupId !== 0)
				)
					throw new Error("FETCH subgroups and datagrams are unsupported");
				const sequence =
					object.group === undefined ? prior : prior === undefined ? object.group : prior + object.group + 1n;
				if (sequence === undefined || sequence > 0xffff_ffff_ffff_ffffn)
					throw new ProtocolViolation("invalid FETCH Group ID");
				if (this.#current?.sequence !== sequence) {
					this.#open(sequence);
					next = 0;
				}
				const id =
					object.group === undefined
						? object.object === undefined
							? next
							: next - 1 + object.object
						: object.object;
				if (id === undefined || id !== next || !Number.isSafeInteger(id))
					throw new Error("FETCH object gap is unsupported");
				if (sequence === this.last && this.bound !== undefined && id >= this.bound)
					throw new ProtocolViolation("FETCH continued past FETCH_OK's End Location");
				if (this.#timescale !== undefined && object.timestamp === undefined)
					throw new StreamError(StreamCode.MalformedTrack, {
						message: `object without a Timestamp on a track with TIMESCALE: group=${sequence}`,
					});
				const producer = this.#current?.producer;
				if (producer) {
					if (object.payload) producer.writeFrame({ payload: object.payload, timestamp: object.timestamp });
					else if (!(await untilAborted(receiveFrame(stream, producer, object), this.#stop.signal)))
						throw new Error("FETCH group was abandoned");
				} else if (!object.payload) await untilAborted(skipPayload(stream, object.size), this.#stop.signal);
				prior = sequence;
				next = id + 1;
				first = false;
			}
			if (
				this.#legacy &&
				this.bound !== undefined &&
				(this.#current?.sequence !== this.last || next < this.bound)
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
