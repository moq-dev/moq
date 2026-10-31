import { expect, test } from "bun:test";
import * as Epoch from "../epoch.ts";
import { ProtocolViolation } from "../error.ts";
import { HopSchema } from "../hop.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import type * as Cluster from "./cluster.ts";
import * as EpochExt from "./epoch.ts";
import { Fetch } from "./fetch.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import { type AdvertParams, PublishNamespace } from "./publish_namespace.ts";
import { SetupOptions } from "./setup_options.ts";
import { Subscribe } from "./subscribe.ts";
import { SubscribeNamespaceEntry } from "./subscribe_namespace.ts";
import { TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

const VERSION = Version.DRAFT_18;
const EPOCH_PARAM = 0x40b69n;
const EPOCH = Epoch.mint();

const ALL = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;

async function encode(
	message: { encode(w: Writer, v: IetfVersion): Promise<void> },
	version: IetfVersion = VERSION,
): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		version,
	);
	await message.encode(writer, version);
	writer.close();
	await writer.closed;

	const out = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.byteLength, 0));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return out;
}

function reader(bytes: Uint8Array, version: IetfVersion = VERSION): Reader {
	return new Reader(undefined, bytes, version);
}

/** A parameter block carrying `value` under the EPOCH key, valid or not. */
function epochParam(value: Uint8Array): Parameters {
	const params = new Parameters();
	params.bytes.set(EPOCH_PARAM, value);
	return params;
}

test("Epoch: SETUP declares it", async () => {
	for (const version of ALL) {
		const params = new SetupOptions();
		expect(EpochExt.fromSetup(params)).toBe(false);
		EpochExt.intoSetup(params);
		const decoded = await SetupOptions.decode(reader(await encode(params, version), version));
		expect(EpochExt.fromSetup(decoded)).toBe(true);
	}
});

test("Epoch: the parameter is the UUID's 16 bytes, length-prefixed", async () => {
	const params = new Parameters();
	params.epoch = EPOCH;
	expect(Array.from(await encode(params))).toEqual([
		0x01, // parameter count
		0xc4,
		0x0b,
		0x69, // EPOCH (0x40b69), delta encoded from 0
		0x10, // value length
		...Epoch.toBytes(EPOCH),
	]);

	const decoded = await Parameters.decode(reader(await encode(params)), VERSION, "subscribe");
	expect(decoded.epoch).toBe(EPOCH);
});

test("Epoch: anything but a UUIDv7's 16 bytes is a protocol violation", async () => {
	const v4 = Epoch.toBytes(EPOCH);
	v4[6] = (v4[6] & 0x0f) | 0x40;

	for (const value of [new Uint8Array(), Epoch.toBytes(EPOCH).slice(0, 15), v4]) {
		const encoded = await encode(epochParam(value));
		await expect(Parameters.decode(reader(encoded), VERSION, "subscribe")).rejects.toThrow(ProtocolViolation);
	}
});

test("Epoch: a REQUEST_UPDATE never carries one", async () => {
	const encoded = await encode(epochParam(Epoch.toBytes(EPOCH)));
	for (const message of ["request-update", "subscribe-update"] as const) {
		await expect(Parameters.decode(reader(encoded), VERSION, message)).rejects.toThrow(ProtocolViolation);
	}
});

test("Epoch: SUBSCRIBE and TRACK_STATUS carry it", async () => {
	const trackNamespace = Path.from("alice");
	for (const version of ALL) {
		const expected = EPOCH;

		const subscribe = new Subscribe({
			requestId: 0n,
			trackNamespace,
			trackName: "video",
			subscriberPriority: 0,
			epoch: EPOCH,
		});
		expect((await Subscribe.decode(reader(await encode(subscribe, version), version), version)).epoch).toBe(
			expected,
		);

		const status = new TrackStatusRequest({ requestId: 0n, trackNamespace, trackName: "video", epoch: EPOCH });
		expect((await TrackStatusRequest.decode(reader(await encode(status, version), version), version)).epoch).toBe(
			expected,
		);
	}
});

test("Epoch: a joining FETCH that names one is malformed", async () => {
	const joining = (params: Parameters) => ({
		encode: (w: Writer, version: IetfVersion) =>
			Message.encode(w, async (mw) => {
				await mw.u62(0n); // request id
				await mw.u53(0x2); // relative joining
				await mw.u62(0n); // joining request id
				await mw.u62(0n); // group offset
				await params.encode(mw, version);
			}),
	});

	// Control: the same FETCH without it decodes.
	expect((await Fetch.decode(reader(await encode(joining(new Parameters()))), VERSION)).epoch).toBeUndefined();

	const named = new Parameters();
	named.epoch = EPOCH;
	await expect(Fetch.decode(reader(await encode(joining(named))), VERSION)).rejects.toThrow(
		"EPOCH is not allowed on a joining FETCH",
	);
});

test("Epoch: PUBLISH_NAMESPACE carries it only once negotiated", async () => {
	const encoded = await encode(
		new PublishNamespace({ requestId: 1n, trackNamespace: Path.from("alice"), params: { epoch: EPOCH } }),
	);

	const decoded = await PublishNamespace.decode(reader(encoded), VERSION, { cluster: false, epoch: true });
	expect(decoded.params).toEqual({ cluster: undefined, epoch: EPOCH });

	// A session that did not negotiate it was never sent one.
	await expect(PublishNamespace.decode(reader(encoded), VERSION)).rejects.toThrow(ProtocolViolation);

	// An advertisement may lack one: the route had no epoch to carry.
	const bare = await encode(new PublishNamespace({ requestId: 1n, trackNamespace: Path.from("alice") }));
	expect(
		(await PublishNamespace.decode(reader(bare), VERSION, { cluster: false, epoch: true })).params.epoch,
	).toBeUndefined();
});

test.each([
	["neither", false, false],
	["cluster only", true, false],
	["epoch only", false, true],
	["both", true, true],
] as const)("Epoch: NAMESPACE framing with %s negotiated", async (_name, cluster, epoch) => {
	const negotiated = { cluster, epoch };
	const advert: Cluster.Advert = { hops: [HopSchema.parse(7n)], cost: 0n };
	const params: AdvertParams = { cluster: cluster ? advert : undefined, epoch: epoch ? EPOCH : undefined };
	const extended = cluster || epoch;

	const entry = new SubscribeNamespaceEntry({ suffix: Path.from("alice"), params: extended ? params : undefined });
	const encoded = await encode(entry);
	const decoded = await SubscribeNamespaceEntry.decode(reader(encoded), VERSION, negotiated);
	expect(decoded.suffix).toBe(Path.from("alice"));
	expect(decoded.params).toEqual(extended ? params : undefined);

	// The block is present iff either extension is, so the base form is strictly shorter.
	const base = await encode(new SubscribeNamespaceEntry({ suffix: Path.from("alice") }));
	expect(base.length < encoded.length).toBe(extended);
});

test("Epoch: a NAMESPACE carrying one without MoQ Epoch is a protocol violation", async () => {
	const advert: Cluster.Advert = { hops: [HopSchema.parse(7n)], cost: 0n };
	const encoded = await encode(
		new SubscribeNamespaceEntry({ suffix: Path.from("alice"), params: { cluster: advert, epoch: EPOCH } }),
	);
	await expect(
		SubscribeNamespaceEntry.decode(reader(encoded), VERSION, { cluster: true, epoch: false }),
	).rejects.toThrow(ProtocolViolation);
});
