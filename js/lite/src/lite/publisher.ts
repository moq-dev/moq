import { type Dispose, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast.ts";
import type { Group } from "../group.ts";
import * as Path from "../path.ts";
import { type Stream, Writer } from "../stream.ts";
import type { Track } from "../track.ts";
import { error } from "../util/error.ts";
import { Announce, AnnounceInit, type AnnounceInterest } from "./announce.ts";
import { Group as GroupMessage } from "./group.ts";
import { encodeSubscribeResponse, type Subscribe, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import { Version } from "./version.ts";

/**
 * Handles publishing broadcasts and managing their lifecycle.
 *
 * @internal
 */
export class Publisher {
	// The version of the connection.
	readonly version: Version;

	#quic: WebTransport;

	// Our published broadcasts.
	// It's a signal so we can live update any announce streams.
	#broadcasts = new Signal<Map<Path.Valid, Broadcast> | undefined>(new Map());

	/**
	 * Creates a new Publisher instance.
	 * @param quic - The WebTransport session to use
	 *
	 * @internal
	 */
	constructor(quic: WebTransport, version: Version) {
		this.#quic = quic;
		this.version = version;
	}

	/**
	 * Publishes a broadcast with any associated tracks.
	 * @param name - The broadcast to publish
	 */
	publish(path: Path.Valid, broadcast: Broadcast) {
		this.#broadcasts.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("closed");
			broadcasts.set(path, broadcast);
		});

		// Remove the broadcast from the lookup when it's closed.
		void broadcast.closed.finally(() => {
			this.#broadcasts.mutate((broadcasts) => {
				broadcasts?.delete(path);
			});
		});
	}

	/**
	 * Handles an announce interest message.
	 * @param msg - The announce interest message
	 * @param stream - The stream to write announcements to
	 *
	 * @internal
	 */
	async runAnnounce(msg: AnnounceInterest, stream: Stream) {
		console.debug(`announce: prefix=${msg.prefix}`);

		// Send initial announcements
		let active = new Set<Path.Valid>();

		const broadcasts = this.#broadcasts.peek();
		if (!broadcasts) return; // closed

		for (const name of broadcasts.keys()) {
			const suffix = Path.stripPrefix(msg.prefix, name);
			if (suffix === null) continue;
			console.debug(`announce: broadcast=${name} active=true`);
			active.add(suffix);
		}

		switch (this.version) {
			case Version.DRAFT_03:
				// Draft03: send individual Announce messages for initial state.
				for (const suffix of active) {
					const wire = new Announce({ suffix, active: true });
					await wire.encode(stream.writer, this.version);
				}
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02: {
				const init = new AnnounceInit([...active]);
				await init.encode(stream.writer, this.version);
				break;
			}
		}

		// Wait for updates to the broadcasts.
		for (;;) {
			// TODO Make a better helper within Signals.
			let dispose!: Dispose;
			const changed = new Promise<Map<Path.Valid, Broadcast> | undefined>((resolve) => {
				dispose = this.#broadcasts.changed(resolve);
			});

			// Wait until the map of broadcasts changes.
			const broadcasts = await Promise.race([changed, stream.reader.closed]);
			dispose();
			if (!broadcasts) break;

			// Create a new set of active broadcasts.
			// This is SLOW, but it's not worth optimizing because we often have just 1 broadcast anyway.
			const newActive = new Set<Path.Valid>();
			for (const name of broadcasts.keys()) {
				const suffix = Path.stripPrefix(msg.prefix, name);
				if (suffix === null) continue; // Not our prefix.
				newActive.add(suffix);
			}

			// Announce any new broadcasts.
			for (const added of newActive.difference(active)) {
				console.debug(`announce: broadcast=${added} active=true`);
				const wire = new Announce({ suffix: added, active: true });
				await wire.encode(stream.writer, this.version);
			}

			// Announce any removed broadcasts.
			for (const removed of active.difference(newActive)) {
				console.debug(`announce: broadcast=${removed} active=false`);
				const wire = new Announce({ suffix: removed, active: false });
				await wire.encode(stream.writer, this.version);
			}

			// NOTE: This is kind of a hack that won't work with a rapid UNANNOUNCE/ANNOUNCE cycle.
			// However, our client doesn't do that anyway.

			active = newActive;
		}
	}

	/**
	 * Handles a subscribe message.
	 * @param msg - The subscribe message
	 * @param stream - The stream to write track data to
	 *
	 * @internal
	 */
	async runSubscribe(msg: Subscribe, stream: Stream) {
		const broadcast = this.#broadcasts.peek()?.get(msg.broadcast);
		if (!broadcast) {
			console.debug(`publish unknown: broadcast=${msg.broadcast}`);
			stream.writer.reset(new Error("not found"));
			return;
		}

		const track = broadcast.subscribe(msg.track, msg.priority);

		try {
			const info = new SubscribeOk({ priority: msg.priority });
			await encodeSubscribeResponse(stream.writer, { ok: info }, this.version);

			console.debug(`publish ok: broadcast=${msg.broadcast} track=${track.name}`);

			const serving = this.#runTrack(msg.id, msg.broadcast, track, stream.writer);

			for (;;) {
				const decode = SubscribeUpdate.decodeMaybe(stream.reader, this.version);

				const result = await Promise.any([serving, decode]);
				if (!result) break;

				if (result instanceof SubscribeUpdate) {
					// TODO use the update
					console.warn("subscribe update not supported", result);
				}
			}

			console.debug(`publish done: broadcast=${msg.broadcast} track=${track.name}`);
			stream.close();
			track.close();
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${e.message}`);
			track.close(e);
			stream.abort(e);
		}
	}

	/**
	 * Runs a track and sends its data to the stream.
	 * @param sub - The subscription ID
	 * @param broadcast - The broadcast name
	 * @param track - The track to run
	 * @param stream - The stream to write to
	 *
	 * @internal
	 */
	async #runTrack(sub: bigint, broadcast: Path.Valid, track: Track, stream: Writer) {
		try {
			for (;;) {
				const next = track.nextGroup();
				const group = await Promise.race([next, stream.closed]);
				if (!group) {
					next.then((group) => group?.close()).catch(() => {});
					break;
				}

				void this.#runGroup(sub, group);
			}

			console.debug(`publish close: broadcast=${broadcast} track=${track.name}`);
			track.close();
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`publish error: broadcast=${broadcast} track=${track.name} error=${e.message}`);
			track.close(e);
			stream.reset(e);
		}
	}

	/**
	 * Runs a group and sends its frames to the stream.
	 * @param sub - The subscription ID
	 * @param group - The group to run
	 *
	 * @internal
	 */
	async #runGroup(sub: bigint, group: Group) {
		const msg = new GroupMessage(sub, group.sequence);
		try {
			const stream = await Writer.open(this.#quic);
			await stream.u8(0); // stream type
			await msg.encode(stream);

			try {
				for (;;) {
					const frame = await Promise.race([group.readFrame(), stream.closed]);
					if (!frame) break;

					await stream.u53(frame.byteLength);
					await stream.write(frame);
				}

				stream.close();
				group.close();
			} catch (err: unknown) {
				const e = error(err);
				stream.reset(e);
				group.close(e);
			}
		} catch (err: unknown) {
			const e = error(err);
			group.close(e);
		}
	}

	close() {
		this.#broadcasts.update((broadcasts) => {
			for (const broadcast of broadcasts?.values() ?? []) {
				broadcast.close();
			}
			return undefined;
		});
	}
}
