import { Effect } from "@moq/signals";
import { Announced } from "../announced.ts";
import { Broadcast } from "../broadcast.ts";
import { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Reader, Stream } from "../stream.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import { Announce, AnnounceInit, AnnounceInterest } from "./announce.ts";
import type { Group as GroupMessage } from "./group.ts";
import { StreamId } from "./stream.ts";
import { Subscribe, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import type { Version } from "./version.ts";

/**
 * Handles subscribing to broadcasts and managing their lifecycle.
 *
 * @internal
 */
export class Subscriber {
	#quic: WebTransport;

	// The version of the connection.
	readonly version: Version;

	// Our subscribed tracks.
	#subscribes = new Map<bigint, Track>();
	#subscribeNext = 0n;

	#effect = new Effect();

	/**
	 * Creates a new Subscriber instance.
	 * @param quic - The WebTransport session to use
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, version: Version) {
		this.#quic = quic;
		this.version = version;
	}

	/**
	 */
	announced(prefix = Path.empty()): Announced {
		const announced = new Announced();
		void this.#runAnnounced(announced, prefix);
		return announced;
	}

	async #runAnnounced(announced: Announced, prefix: Path.Valid): Promise<void> {
		console.debug(`announced: prefix=${prefix}`);
		const msg = new AnnounceInterest(prefix);

		try {
			// Open a stream and send the announce interest.
			const stream = await Stream.open(this.#quic);
			await stream.writer.u53(StreamId.Announce);
			await msg.encode(stream.writer);

			// First, receive ANNOUNCE_INIT
			const init = await AnnounceInit.decode(stream.reader);

			// Process initial announcements
			for (const suffix of init.suffixes) {
				const path = Path.join(prefix, suffix);
				console.debug(`announced: broadcast=${path} active=true`);
				announced.append({ path, active: true });
			}

			// Then receive updates
			for (;;) {
				const announce = await Promise.race([Announce.decodeMaybe(stream.reader), announced.closed]);
				if (!announce) break;
				if (announce instanceof Error) throw announce;

				const path = Path.join(prefix, announce.suffix);

				console.debug(`announced: broadcast=${path} active=${announce.active}`);
				announced.append({ path, active: announce.active });
			}

			announced.close();
		} catch (err: unknown) {
			announced.close(error(err));
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * @param name - The name of the broadcast to consume
	 * @returns A Broadcast instance
	 */
	consume(path: Path.Valid): Broadcast {
		const broadcast = new Broadcast();

		(async () => {
			for (;;) {
				const request = await broadcast.requested();
				console.log("request", request);
				if (!request) break;
				this.#runSubscribe(path, request);
			}
		})();

		return broadcast;
	}

	async #runSubscribe(broadcast: Path.Valid, track: Track) {
		const id = this.#subscribeNext++;

		// Save the writer so we can append groups to it.
		this.#subscribes.set(id, track);

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${track.name}`);

		const msg = new Subscribe({
			id,
			broadcast,
			track: track.name,
			priority: track.priority.peek(),
			maxLatency: track.maxLatency.peek(),
		});

		const stream = await Stream.open(this.#quic);
		await stream.writer.u53(StreamId.Subscribe);

		await msg.encode(stream.writer, this.version);

		try {
			await SubscribeOk.decode(stream.reader, this.version);
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${track.name}`);

			const closed = track.closed.promise();

			for (;;) {
				// TODO this is super gross and needs to be refactored.
				// Listen for changes to the priority and max latency and send an update message.
				let dispose!: () => void;
				const updated = new Promise<boolean>((resolve) => {
					const d1 = track.priority.changed(() => resolve(true));
					const d2 = track.maxLatency.changed(() => resolve(true));

					dispose = () => {
						d1();
						d2();
					};
				});

				const result = await Promise.race([stream.reader.closed, closed, updated]);
				dispose();

				if (result === true) {
					const update = new SubscribeUpdate({
						priority: track.priority.peek(),
						maxLatency: track.maxLatency.peek(),
					});
					await update.encode(stream.writer, this.version);
				} else if (result instanceof Error) {
					throw result;
				} else {
					break;
				}
			}

			track.close();
			stream.close();
			console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${track.name}`);
		} catch (err) {
			const e = error(err);
			track.close(e);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${track.name} error=${e.message}`);
			stream.abort(e);
		} finally {
			this.#subscribes.delete(id);
		}
	}

	/**
	 * Handles a group message.
	 * @param group - The group message
	 * @param stream - The stream to read frames from
	 *
	 * @internal
	 */
	async runGroup(group: GroupMessage, stream: Reader) {
		const subscribe = this.#subscribes.get(group.subscribe);
		if (!subscribe) {
			if (group.subscribe >= this.#subscribeNext) {
				throw new Error(`unknown subscription: id=${group.subscribe}`);
			}

			return;
		}

		const producer = new Group(group.sequence);
		subscribe.writeGroup(producer);

		try {
			for (;;) {
				const done = await Promise.race([stream.done(), subscribe.closed, producer.closed]);
				if (done !== false) break;

				const size = await stream.u53();
				const payload = await stream.read(size);
				if (!payload) break;

				producer.writeFrame(payload);
			}

			producer.close();
			stream.stop(new Error("cancel"));
		} catch (err: unknown) {
			const e = error(err);
			producer.close(e);
			stream.stop(e);
		}
	}

	close() {
		for (const track of this.#subscribes.values()) {
			track.close();
		}

		this.#subscribes.clear();
		this.#effect.close();
	}
}
