import { expect, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Nano } from "@moq/clock";
import Qmux from "@moq/qmux";
import { Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { Tracks } from "../broadcast.ts";
import { StreamCode, StreamError } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as Origin } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Group } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { Publish, PublishDone, PublishDoneStatus } from "./publish.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscribe } from "./subscribe.ts";
import { PublishSkipped, SubscribeTracks } from "./subscribe_tracks.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const drafts = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22];

/** A client and server WebTransport, connected to each other. */
type Pair = { client: WebTransport; server: WebTransport };

/**
 * A qmux session pair over an in-memory socket, which, unlike the mock transport, holds each
 * bidi stream against the receiver's stream credit until both its halves end. The client
 * grants the server `credit` such streams at once.
 */
function qmuxPair(credit: bigint): Pair {
	const socket = (readable: ReadableStream<Uint8Array>, writable: WritableStream<Uint8Array>) => {
		const closed = Promise.withResolvers<{ closeCode: number; reason: string }>();
		return {
			url: "",
			opened: Promise.resolve({ readable, writable, extensions: "", protocol: "qmux-01" }),
			closed: closed.promise,
			close: () => closed.resolve({ closeCode: 1000, reason: "" }),
		};
	};
	// Copied, since a sender may reuse its buffer once a write resolves.
	const pipe = () =>
		new TransformStream<Uint8Array, Uint8Array>({ transform: (chunk, c) => c.enqueue(chunk.slice()) });
	const up = pipe();
	const down = pipe();
	const client = new Qmux(socket(down.readable, up.writable), {
		config: { idleTimeout: null, maxStreamsBidi: credit },
	});
	const server = Qmux.accept(socket(up.readable, down.writable), { config: { idleTimeout: null } });
	return { client, server };
}

function fixture(
	version: IetfVersion,
	{
		grant = new Signal<Grant | undefined>(undefined),
		pair = createMockTransportPair(ALPN.DRAFT_22),
	}: { grant?: Signal<Grant | undefined>; pair?: Pair } = {},
) {
	const origin = new Origin();
	const session = new NativeSession(pair.server, version, false);
	const publisher = new Publisher({
		quic: pair.server,
		session,
		publish: origin.consume(),
		requiresSolicitation: true,
		grant,
	});
	const tasks: Promise<void>[] = [];
	const streams: Stream[] = [];
	onTestFinished(async () => {
		for (const stream of streams) stream.abort(new StreamError(StreamCode.Cancel, { message: "test ended" }));
		origin.close();
		pair.server.close();
		pair.client.close();
		publisher.close();
		await Promise.all(tasks);
	});
	return {
		pair,
		origin,
		publisher,
		session,
		tasks,
		streams,
		async list(prefix = "room", forward = false, parameters = new Parameters()) {
			const client = await Stream.open(pair.client, { version });
			streams.push(client);
			parameters.forward = forward;
			// Sent, since a transport that opens a stream lazily shows the peer none until then.
			await Message.write(
				client.writer,
				SubscribeTracks.id,
				new SubscribeTracks(0n, Path.from(prefix), parameters),
				version,
			);
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("missing listing stream");
			expect(await server.reader.u53()).toBe(SubscribeTracks.id);
			tasks.push(publisher.runSubscribeTracks(await SubscribeTracks.decode(server.reader, version), server));
			return client;
		},
		/** The next PUBLISH, answered with a paused PUBLISH_OK unless `answer` is false. */
		async offered(answer = true) {
			const stream = await Stream.accept(pair.client, version);
			if (!stream) throw new Error("missing PUBLISH");
			streams.push(stream);
			expect(await stream.reader.u53()).toBe(Publish.id);
			const publish = await Publish.decode(stream.reader, version);
			expect(publish.forward).toBe(false);
			if (answer) await publishOk(stream, version);
			return { stream, publish };
		},
	};
}

async function publishOk(stream: Stream, version: IetfVersion) {
	const parameters = new Parameters();
	if (version < Version.DRAFT_20) parameters.forward = false;
	await stream.writer.u53(RequestOk.id);
	await new RequestOk({ parameters }).encode(stream.writer, version);
}

async function ok(stream: Stream, version: IetfVersion) {
	expect(await stream.reader.u53()).toBe(RequestOk.id);
	await RequestOk.decode(stream.reader, version);
}

async function update(stream: Stream, version: IetfVersion, id: bigint, forward: boolean) {
	await stream.writer.u53(0x02);
	await Message.encode(stream.writer, async (body) => {
		await body.u62(id);
		const parameters = new Parameters();
		parameters.forward = forward;
		await parameters.encode(body, version);
	});
	await ok(stream, version);
}

for (const version of drafts) {
	test(`track listing pauses, resumes, survives listing cancellation and ends on withdrawal (${version})`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		using _installed = { [Symbol.dispose]: () => installed.close() };
		const fx = fixture(version);
		const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const track = broadcast.createTrack("video");
		const group = track.appendGroup();
		group.writeFrame({ payload: new Uint8Array([1]) });
		group.finish();
		const listing = await fx.list();
		await ok(listing, version);
		const { stream, publish } = await fx.offered();
		expect(publish.trackNamespace).toBe(Path.from("room/live"));
		expect(publish.trackName).toBe("video");
		const uni = fx.pair.client.incomingUnidirectionalStreams.getReader();
		let delivered = false;
		const first = uni.read();
		void first.then(() => {
			delivered = true;
		});
		await clock.advance(Nano.zero);
		expect(delivered).toBe(false);
		await update(stream, version, 2n, true);
		const next = await first;
		if (!next.value) throw new Error("missing group");
		const reader = new Reader(next.value, undefined, version);
		expect((await Group.decode(reader, version)).trackAlias).toBe(publish.trackAlias);
		await reader.readAll();
		await update(stream, version, 4n, false);
		listing.abort(new StreamError(StreamCode.Cancel, { message: "listing cancelled" }));
		await clock.advance(Nano.zero);
		await update(stream, version, 6n, true);
		const resumed = await uni.read();
		if (!resumed.value) throw new Error("missing resumed group");
		await new Reader(resumed.value, undefined, version).readAll();
		await update(stream, version, 8n, false);
		broadcast.close();
		expect(await stream.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(stream.reader, version);
		expect(done.statusCode).toBe(PublishDoneStatus.INTERNAL_ERROR);
		expect(done.streamCount).toBe(2n);
		uni.releaseLock();
	});

	// The PUBLISH ends as its track left the listing: TRACK_ENDED once it finished, in error
	// otherwise, since a track removed unfinished may go on elsewhere.
	for (const finished of [true, false]) {
		test(`an unlisted track's PUBLISH ends ${finished ? "with TRACK_ENDED" : "in error"} (${version})`, async () => {
			const fx = fixture(version);
			const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
			const broadcast = broadcastPublication.broadcast;
			broadcastPublication.announce();
			const track = broadcast.createTrack("video");
			const listing = await fx.list();
			await ok(listing, version);
			const { stream } = await fx.offered();
			if (finished) track.finish();
			if (finished) track.close();
			broadcast.removeTrack("video");
			expect(await stream.reader.u53()).toBe(PublishDone.id);
			const done = await PublishDone.decode(stream.reader, version);
			expect(done.statusCode).toBe(finished ? PublishDoneStatus.TRACK_ENDED : PublishDoneStatus.INTERNAL_ERROR);
		});
	}

	// The peer listed the track on its PUBLISH, so one that finishes before the answer still
	// ends with TRACK_ENDED once answered, rather than a reset that fails the peer's listing.
	test(`a track finished before its PUBLISH is answered ends with TRACK_ENDED (${version})`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		using _installed = { [Symbol.dispose]: () => installed.close() };
		const fx = fixture(version);
		const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const track = broadcast.createTrack("video");
		const listing = await fx.list();
		await ok(listing, version);
		const { stream } = await fx.offered(false);
		track.finish();
		track.close();
		broadcast.removeTrack("video");
		await clock.advance(Nano.zero);
		await publishOk(stream, version);
		expect(await stream.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(stream.reader, version);
		expect(done.statusCode).toBe(PublishDoneStatus.TRACK_ENDED);
	});

	// The PUBLISH may reach the peer before its write returns, as when the write budget yields
	// after the transport took the bytes. A track that finishes then is listed already.
	test(`a track finished as its PUBLISH goes out ends with TRACK_ENDED (${version})`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		using _installed = { [Symbol.dispose]: () => installed.close() };
		// Spied before listing, since the PUBLISH may go out before the listing's OK is read.
		// Restored only once the test is done: `Message.write` reads `writer.write` before
		// awaiting its frame, so restoring mid-write would drop a concurrent write on the stale spy.
		const resume = Promise.withResolvers<void>();
		let holding = false;
		const write = Writer.prototype.write;
		const held = spyOn(Writer.prototype, "write").mockImplementation(async function (this: Writer, chunk) {
			await write.call(this, chunk);
			if (holding || chunk[0] !== Publish.id) return;
			holding = true;
			await resume.promise;
		});
		using _held = { [Symbol.dispose]: () => held.mockRestore() };
		const fx = fixture(version);
		const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const track = broadcast.createTrack("video");
		const listing = await fx.list();
		await ok(listing, version);
		const { stream } = await fx.offered(false);
		track.finish();
		track.close();
		broadcast.removeTrack("video");
		await clock.advance(Nano.zero);
		expect(holding).toBe(true);
		resume.resolve();
		await publishOk(stream, version);
		expect(await stream.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(stream.reader, version);
		expect(done.statusCode).toBe(PublishDoneStatus.TRACK_ENDED);
	});

	// A broadcast closing right after a track finished ends that track's PUBLISH with TRACK_ENDED,
	// removed first or not and answered or not, while a track left unfinished still ends in error:
	// PUBLISH_DONE once answered, a reset before.
	for (const remove of [false, true]) {
		for (const answer of [true, false]) {
			const name = `remove=${remove} answer=${answer}`;
			test(`a clean end keeps its finishes, ${name} (${version})`, async () => {
				const clock = new Manual();
				const installed = clock.install();
				using _installed = { [Symbol.dispose]: () => installed.close() };
				const fx = fixture(version);
				const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
				const broadcast = broadcastPublication.broadcast;
				broadcastPublication.announce();
				const video = broadcast.createTrack("video");
				broadcast.createTrack("audio");
				const listing = await fx.list();
				await ok(listing, version);
				const published = new Map<string, Stream>();
				for (let i = 0; i < 2; i++) {
					const { stream, publish } = await fx.offered(answer);
					published.set(publish.trackName, stream);
				}
				await clock.advance(Nano.zero);
				video.finish();
				video.close();
				if (remove) broadcast.removeTrack("video");
				broadcast.close();
				await clock.advance(Nano.zero);
				const videoStream = published.get("video");
				const audioStream = published.get("audio");
				if (!videoStream || !audioStream) throw new Error("missing PUBLISH");
				if (!answer) await publishOk(videoStream, version);
				expect(await videoStream.reader.u53()).toBe(PublishDone.id);
				expect((await PublishDone.decode(videoStream.reader, version)).statusCode).toBe(
					PublishDoneStatus.TRACK_ENDED,
				);
				if (!answer) {
					await expect(audioStream.reader.u53()).rejects.toThrow();
					return;
				}
				expect(await audioStream.reader.u53()).toBe(PublishDone.id);
				expect((await PublishDone.decode(audioStream.reader, version)).statusCode).toBe(
					PublishDoneStatus.INTERNAL_ERROR,
				);
			});
		}
	}

	test(`track listing rejects overlapping prefixes (${version})`, async () => {
		const fx = fixture(version);
		const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		broadcast.createTrack("video");
		const listing = await fx.list();
		await ok(listing, version);
		await fx.offered();
		for (const prefix of ["room/live", "", "room"]) {
			const overlap = await fx.list(prefix);
			expect(await overlap.reader.u53()).toBe(RequestError.id);
			expect((await RequestError.decode(overlap.reader, version)).errorCode).toBe(0x30);
		}
	});

	test(`forwarding listings are skipped or refused according to the draft (${version})`, async () => {
		const fx = fixture(version);
		const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		broadcast.createTrack("video");
		const listing = await fx.list("room", true);
		if (version === Version.DRAFT_18) {
			expect(await listing.reader.u53()).toBe(RequestError.id);
			expect((await RequestError.decode(listing.reader, version)).errorCode).toBe(3);
		} else {
			await ok(listing, version);
			expect(await listing.reader.u53()).toBe(PublishSkipped.id);
			const skipped = await PublishSkipped.decode(listing.reader, version);
			expect(skipped.suffix).toBe(Path.from("live"));
			expect(skipped.trackName).toBe("video");
		}
	});
}

test("draft-18 refuses SUBSCRIBE to a PUBLISHed track", async () => {
	const version = Version.DRAFT_18;
	const fx = fixture(version);
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("video");
	const listing = await fx.list();
	await ok(listing, version);
	await fx.offered();
	const client = await Stream.open(fx.pair.client, { version });
	const server = await Stream.accept(fx.pair.server, version);
	if (!server) throw new Error("missing subscribe");
	fx.streams.push(client);
	fx.tasks.push(
		fx.publisher.runSubscribe(
			new Subscribe({
				requestId: 2n,
				trackNamespace: Path.from("room/live"),
				trackName: "video",
				subscriberPriority: 127,
			}),
			server,
		),
	);
	expect(await client.reader.u53()).toBe(RequestError.id);
	expect((await RequestError.decode(client.reader, version)).errorCode).toBe(0x19);
});

test("a cancelled listing cannot release its replacement's prefix", async () => {
	const version = Version.DRAFT_22;
	const clock = new Manual();
	const installed = clock.install();
	using _installed = { [Symbol.dispose]: () => installed.close() };
	const fx = fixture(version);
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("video");
	const first = await fx.list();
	await ok(first, version);
	const old = await fx.offered();
	first.abort(new StreamError(StreamCode.Cancel));
	await clock.advance(Nano.zero);
	const second = await fx.list();
	await ok(second, version);
	await fx.offered();
	old.stream.abort(new StreamError(StreamCode.Cancel));
	await fx.tasks[0];
	const overlap = await fx.list();
	expect(await overlap.reader.u53()).toBe(RequestError.id);
	expect((await RequestError.decode(overlap.reader, version)).errorCode).toBe(0x30);
});

test("grants hide tracks, revoke PUBLISHes and relist when broadened", async () => {
	const version = Version.DRAFT_22;
	const empty = (): Grant => ({ publish: new Path.Patterns([]), subscribe: new Path.Patterns([]) });
	const grant = new Signal<Grant | undefined>(empty());
	const clock = new Manual();
	const installed = clock.install();
	using _installed = { [Symbol.dispose]: () => installed.close() };
	const fx = fixture(version, { grant });
	const listing = await fx.list();
	await ok(listing, version);
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("video");
	let delivered = false;
	const offered = fx.offered();
	void offered.then(() => {
		delivered = true;
	});
	await clock.advance(Nano.zero);
	expect(delivered).toBe(false);
	grant.set(undefined);
	const first = await offered;
	grant.set(empty());
	expect(await first.stream.reader.u53()).toBe(PublishDone.id);
	expect((await PublishDone.decode(first.stream.reader, version)).statusCode).toBe(PublishDoneStatus.UNAUTHORIZED);
	grant.set(undefined);
	expect((await fx.offered()).publish.trackName).toBe("video");
});

test("PUBLISH carries listing defaults and honors INCLUDE_PROPERTIES", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version);
	const parameters = new Parameters();
	parameters.subscriberPriority = 42;
	parameters.groupOrder = 1;
	parameters.subscriptionFilter = { kind: "relative", groups: 2n };
	parameters.includeProperties = false;
	const listing = await fx.list("room", false, parameters);
	await ok(listing, version);
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("later");
	const { publish } = await fx.offered();
	expect(publish.properties).toEqual({});
	expect(publish.parameters?.subscriberPriority).toBe(42);
	expect(publish.parameters?.groupOrder).toBe(1);
	expect(publish.parameters?.subscriptionFilter).toEqual({ kind: "relative", groups: 2n });
});

// An omitted SUBSCRIBER_PRIORITY is the drafts' 128, so the PUBLISH restates 128, not the unset 127.
test("PUBLISH restates an omitted listing priority as 128", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version);
	const listing = await fx.list("room", false, new Parameters());
	await ok(listing, version);
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("later");
	const { publish } = await fx.offered();
	expect(publish.parameters?.subscriberPriority).toBe(128);
});

test("an update ends only its listing and frees its prefix", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version);
	const listing = await fx.list();
	await ok(listing, version);
	await listing.writer.u53(2);
	await Message.encode(listing.writer, async (body) => {
		await body.u62(2n);
		await new Parameters().encode(body, version);
	});
	expect(await listing.reader.u53()).toBe(RequestError.id);
	expect((await RequestError.decode(listing.reader, version)).errorCode).toBe(3);
	await fx.tasks[0];
	const replacement = await fx.list();
	await ok(replacement, version);
});

test("session shutdown releases listing tasks while the local origin stays open", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version);
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	broadcast.createTrack("video");
	const listing = await fx.list();
	await ok(listing, version);
	await fx.offered();
	fx.pair.server.close();
	await Promise.all(fx.tasks);
	expect(broadcast.createTrack("still-local").name).toBe("still-local");
});

for (const version of [Version.DRAFT_22]) {
	test(`a filter applied while paused or while pausing is preserved on resume (${version})`, async () => {
		const fx = fixture(version);
		const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
		const broadcast = broadcastPublication.broadcast;
		broadcastPublication.announce();
		const track = broadcast.createTrack("video");
		for (let i = 0; i < 2; i++) {
			const group = track.appendGroup();
			group.writeFrame({ payload: new Uint8Array([i]) });
			group.finish();
		}
		await ok(await fx.list(), version);
		const { stream } = await fx.offered();
		const filter = async (id: bigint, startGroup: bigint) => {
			await stream.writer.u53(0x02);
			await Message.encode(stream.writer, async (body) => {
				await body.u62(id);
				const params = new Parameters();
				params.forward = false;
				params.subscriptionFilter = { kind: "absolute", startGroup, startObject: 0n };
				await params.encode(body, version);
			});
		};
		await filter(2n, 2n);
		await ok(stream, version);
		await update(stream, version, 4n, true);
		const fresh = track.appendGroup();
		fresh.writeFrame({ payload: new Uint8Array([2]) });
		fresh.finish();
		const uni = fx.pair.client.incomingUnidirectionalStreams.getReader();
		const next = await uni.read();
		if (!next.value) throw new Error("missing filtered group");
		const reader = new Reader(next.value, undefined, version);
		expect((await Group.decode(reader, version)).groupId).toBe(2);
		await reader.readAll();

		// An active pause applies the same filter atomically before its acknowledgement.
		await filter(6n, 3n);
		await ok(stream, version);
		while (track.subscription.peek() !== undefined) await track.subscription.changed();
		const paused = track.appendGroup();
		paused.writeFrame({ payload: new Uint8Array([3]) });
		paused.finish();
		await update(stream, version, 8n, true);
		const resumed = (await uni.read()).value;
		if (!resumed) throw new Error("missing group selected while paused");
		const resumedReader = new Reader(resumed, undefined, version);
		expect((await Group.decode(resumedReader, version)).groupId).toBe(3);
		await resumedReader.readAll();
		uni.releaseLock();
	});
}

/** Listed broadcast sizes to sweep: what a listing holds must not grow with them. */
const CATALOGS = [4, 64, 512];

/** A broadcast under `room` listing `catalog` tracks. */
function catalogOf(fx: ReturnType<typeof fixture>, catalog: number) {
	const broadcastPublication = fx.origin.createBroadcast(Path.from("room/live"));
	const broadcast = broadcastPublication.broadcast;
	broadcastPublication.announce();
	for (let i = 0; i < catalog; i++) broadcast.createTrack(`t${i}`);
}

// Each PUBLISH opens only as the peer's stream credit admits it: a peer that withholds credit and
// reads nothing leaves one stream open waiting, however many tracks are listed, and each track is
// PUBLISHed once credit returns.
for (const version of drafts) {
	test(`a listing publishes only as credit admits (${version})`, async () => {
		const credit = 2;
		const clock = new Manual();
		const installed = clock.install();
		using _installed = { [Symbol.dispose]: () => installed.close() };
		for (const catalog of CATALOGS) {
			const pair = qmuxPair(BigInt(credit));
			const open = pair.server.createBidirectionalStream.bind(pair.server);
			let waiting = 0;
			spyOn(pair.server, "createBidirectionalStream").mockImplementation((options) => {
				waiting += 1;
				const opened = open(options);
				const done = () => {
					waiting -= 1;
				};
				opened.then(done, done);
				return opened;
			});
			const fx = fixture(version, { pair });
			catalogOf(fx, catalog);
			const listing = await fx.list();
			await ok(listing, version);
			const held = [];
			for (let i = 0; i < credit; i++) held.push(await fx.offered(false));
			await clock.advance(Nano.zero);
			expect(waiting, `catalog=${catalog}`).toBeLessThanOrEqual(1);

			// Refusing each PUBLISH returns its credit, which opens the next.
			const published = new Set(held.map(({ publish }) => publish.trackName));
			for (const { stream } of held) stream.abort(new StreamError(StreamCode.Cancel));
			while (published.size < catalog) {
				const { stream, publish } = await fx.offered(false);
				expect(published.has(publish.trackName), "PUBLISHed a track twice").toBe(false);
				published.add(publish.trackName);
				stream.abort(new StreamError(StreamCode.Cancel));
				expect(waiting, `catalog=${catalog}`).toBeLessThanOrEqual(1);
			}
		}
	});
}

// A FORWARD=1 listing reads its next track only once the request stream has taken the last
// skip, so a peer that stops reading leaves one skip queued, however many tracks are listed.
for (const version of [Version.DRAFT_22]) {
	test(`a forwarding listing reads only as skips go out (${version})`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		using _installed = { [Symbol.dispose]: () => installed.close() };
		// Hold each skip's write until the test lets it out, as a peer that stops reading would.
		const gates: (() => void)[] = [];
		let arrived = Promise.withResolvers<void>();
		const write = Message.write;
		const writes = spyOn(Message, "write").mockImplementation(async (writer, id, msg, version) => {
			if (id === PublishSkipped.id)
				await new Promise<void>((resolve) => {
					gates.push(resolve);
					arrived.resolve();
				});
			return write(writer, id, msg, version);
		});
		const gate = async () => {
			while (!gates.length) {
				await arrived.promise;
				arrived = Promise.withResolvers();
			}
			return gates.shift() as () => void;
		};
		const reads = spyOn(Tracks.prototype, "next");
		onTestFinished(() => {
			writes.mockRestore();
			reads.mockRestore();
		});
		for (const catalog of CATALOGS) {
			reads.mockClear();
			const fx = fixture(version);
			catalogOf(fx, catalog);
			const listing = await fx.list("room", true);
			await ok(listing, version);
			const skipped = new Set<string>();
			while (skipped.size < catalog) {
				const release = await gate();
				await clock.advance(Nano.zero);
				// The skip being written, one queued, and the listing's SYNCED or next read.
				expect(reads.mock.calls.length, `catalog=${catalog}`).toBeLessThanOrEqual(skipped.size + 3);
				release();
				expect(await listing.reader.u53()).toBe(PublishSkipped.id);
				const { trackName } = await PublishSkipped.decode(listing.reader, version);
				expect(skipped.has(trackName), "skipped a track twice").toBe(false);
				skipped.add(trackName);
			}
			await clock.advance(Nano.zero);
			expect(gates.length, "skipped past the listing").toBe(0);
		}
	});
}
