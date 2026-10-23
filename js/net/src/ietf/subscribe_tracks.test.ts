import { expect, onTestFinished, spyOn, test } from "bun:test";
import { Manual, Nano } from "@moq/clock";
import { Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { StreamCode, StreamError } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as Origin } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import { NativeSession } from "./adapter.ts";
import * as Message from "./message.ts";
import { Group } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { Publish, PublishDone, PublishDoneStatus } from "./publish.ts";
import { Publisher } from "./publisher.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Subscribe } from "./subscribe.ts";
import { PublishSkipped, SubscribeTracks } from "./subscribe_tracks.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const drafts = [Version.DRAFT_18, Version.DRAFT_19, Version.DRAFT_20, Version.DRAFT_21, Version.DRAFT_22];

function fixture(version: IetfVersion, subscriptions = 10, grant = new Signal<Grant | undefined>(undefined)) {
	const pair = createMockTransportPair(ALPN.DRAFT_22);
	const origin = new Origin();
	const session = new NativeSession(pair.server, version, false);
	const publisher = new Publisher({
		quic: pair.server,
		session,
		publish: origin.consume(),
		requiresSolicitation: true,
		grant,
		limits: { announces: 10, subscriptions, fetches: 10 },
	});
	const tasks: Promise<void>[] = [];
	const streams: Stream[] = [];
	onTestFinished(async () => {
		for (const stream of streams) stream.abort(new StreamError(StreamCode.Cancel, { message: "test ended" }));
		origin.close();
		pair.server.close();
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
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("missing listing stream");
			streams.push(client);
			parameters.forward = forward;
			tasks.push(publisher.runSubscribeTracks(new SubscribeTracks(0n, Path.from(prefix), parameters), server));
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
		using _installed = { [Symbol.dispose]: clock.install() };
		const fx = fixture(version);
		const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
		broadcast.announce();
		const track = broadcast.createTrack("video");
		const group = track.appendGroup();
		group.writeFrame({ payload: new Uint8Array([1]) });
		group.close();
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
			const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
			broadcast.announce();
			const track = broadcast.createTrack("video");
			const listing = await fx.list();
			await ok(listing, version);
			const { stream } = await fx.offered();
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
		using _installed = { [Symbol.dispose]: clock.install() };
		const fx = fixture(version);
		const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
		broadcast.announce();
		const track = broadcast.createTrack("video");
		const listing = await fx.list();
		await ok(listing, version);
		const { stream } = await fx.offered(false);
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
		using _installed = { [Symbol.dispose]: clock.install() };
		const fx = fixture(version);
		const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
		broadcast.announce();
		const track = broadcast.createTrack("video");
		const listing = await fx.list();
		await ok(listing, version);
		const resume = Promise.withResolvers<void>();
		const write = Writer.prototype.write;
		const held = spyOn(Writer.prototype, "write").mockImplementation(async function (this: Writer, chunk) {
			await write.call(this, chunk);
			if (chunk[0] !== Publish.id) return;
			held.mockRestore();
			await resume.promise;
		});
		const { stream } = await fx.offered(false);
		track.close();
		broadcast.removeTrack("video");
		await clock.advance(Nano.zero);
		resume.resolve();
		await publishOk(stream, version);
		expect(await stream.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(stream.reader, version);
		expect(done.statusCode).toBe(PublishDoneStatus.TRACK_ENDED);
	});

	test(`track listing skips beyond the cap and rejects overlapping prefixes (${version})`, async () => {
		const fx = fixture(version, 1);
		const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
		broadcast.announce();
		broadcast.createTrack("video");
		broadcast.createTrack("audio");
		const listing = await fx.list();
		await ok(listing, version);
		await fx.offered();
		expect(await listing.reader.u53()).toBe(PublishSkipped.id);
		const skipped = await PublishSkipped.decode(listing.reader, version);
		expect(skipped.suffix).toBe(Path.from("live"));
		expect(skipped.trackName).toBe("audio");
		for (const prefix of ["room/live", "", "room"]) {
			const overlap = await fx.list(prefix);
			expect(await overlap.reader.u53()).toBe(RequestError.id);
			expect((await RequestError.decode(overlap.reader, version)).errorCode).toBe(0x30);
		}
	});

	test(`forwarding listings are skipped or refused according to the draft (${version})`, async () => {
		const fx = fixture(version);
		const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
		broadcast.announce();
		broadcast.createTrack("video");
		const listing = await fx.list("room", true);
		if (version === Version.DRAFT_18) {
			expect(await listing.reader.u53()).toBe(RequestError.id);
			expect((await RequestError.decode(listing.reader, version)).errorCode).toBe(3);
		} else {
			await ok(listing, version);
			expect(await listing.reader.u53()).toBe(PublishSkipped.id);
			expect((await PublishSkipped.decode(listing.reader, version)).trackName).toBe("video");
		}
	});
}

test("draft-18 refuses SUBSCRIBE to a PUBLISHed track", async () => {
	const version = Version.DRAFT_18;
	const fx = fixture(version);
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
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
	using _installed = { [Symbol.dispose]: clock.install() };
	const fx = fixture(version);
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
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
	using _installed = { [Symbol.dispose]: clock.install() };
	const fx = fixture(version, 10, grant);
	const listing = await fx.list();
	await ok(listing, version);
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
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
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
	broadcast.createTrack("later");
	const { publish } = await fx.offered();
	expect(publish.properties).toEqual({});
	expect(publish.parameters?.subscriberPriority).toBe(42);
	expect(publish.parameters?.groupOrder).toBe(1);
	expect(publish.parameters?.subscriptionFilter).toEqual({ kind: "relative", groups: 2n });
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

test("paused updates keep one slot and release it when the PUBLISH ends", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version, 1);
	const clock = new Manual();
	using _installed = { [Symbol.dispose]: clock.install() };
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
	broadcast.createTrack("video");
	const listing = await fx.list();
	await ok(listing, version);
	const offered = await fx.offered();
	for (let i = 0; i < 32; i++) await update(offered.stream, version, BigInt(2 * (i + 1)), false);
	broadcast.createTrack("skipped");
	expect(await listing.reader.u53()).toBe(PublishSkipped.id);
	expect((await PublishSkipped.decode(listing.reader, version)).trackName).toBe("skipped");
	offered.stream.abort(new StreamError(StreamCode.Cancel));
	await clock.advance(Nano.zero);
	broadcast.createTrack("available");
	expect((await fx.offered()).publish.trackName).toBe("available");
});

test("a failed PUBLISH stream open returns its subscription slot", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version, 1);
	const clock = new Manual();
	using _installed = { [Symbol.dispose]: clock.install() };
	const open = spyOn(fx.session, "openBi").mockRejectedValue(new StreamError(StreamCode.Cancel));
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
	broadcast.createTrack("video");
	const listing = await fx.list();
	await ok(listing, version);
	await clock.advance(Nano.zero);
	expect(open).toHaveBeenCalledTimes(1);
	open.mockRestore();
	broadcast.createTrack("available");
	expect((await fx.offered()).publish.trackName).toBe("available");
});

test("session shutdown releases listing tasks while the local origin stays open", async () => {
	const version = Version.DRAFT_22;
	const fx = fixture(version);
	const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
	broadcast.announce();
	broadcast.createTrack("video");
	const listing = await fx.list();
	await ok(listing, version);
	await fx.offered();
	fx.pair.server.close();
	await Promise.all(fx.tasks);
	expect(broadcast.createTrack("still-local").name).toBe("still-local");
});

for (const version of [Version.DRAFT_20, Version.DRAFT_22]) {
	test(`a paused filter applies on resume, but an active pause/filter update is refused atomically (${version})`, async () => {
		const fx = fixture(version);
		const broadcast = fx.origin.createBroadcast(Path.from("room/live"));
		broadcast.announce();
		const track = broadcast.createTrack("video");
		for (let i = 0; i < 2; i++) {
			const group = track.appendGroup();
			group.writeFrame({ payload: new Uint8Array([i]) });
			group.close();
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
		fresh.close();
		const uni = fx.pair.client.incomingUnidirectionalStreams.getReader();
		const next = await uni.read();
		if (!next.value) throw new Error("missing filtered group");
		const reader = new Reader(next.value, undefined, version);
		expect((await Group.decode(reader, version)).groupId).toBe(2);
		await reader.readAll();

		// The bundled pause never receives REQUEST_OK or leaves a paused publication.
		await filter(6n, 3n);
		expect(await stream.reader.u53()).toBe(RequestError.id);
		expect((await RequestError.decode(stream.reader, version)).errorCode).toBe(0x03);
		expect(await stream.reader.u53()).toBe(PublishDone.id);
		const done = await PublishDone.decode(stream.reader, version);
		expect(done.statusCode).toBe(PublishDoneStatus.UPDATE_FAILED);
		expect(done.streamCount).toBe(1n);
		uni.releaseLock();
	});
}
