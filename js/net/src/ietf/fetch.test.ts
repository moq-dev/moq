import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { Fetch, FetchOk, type FetchType, type Location } from "./fetch.ts";
import type * as Filter from "./filter.ts";
import { type IetfVersion, Version } from "./version.ts";

const D18 = Version.DRAFT_18;
const D22 = Version.DRAFT_22;

const FETCH_ID = 9n;

/**
 * The range `[start, end)` as each draft spells it: a standalone FETCH through draft-18, End
 * Object 0 for the whole End Group, and an inclusive LOCATION_FILTER from draft-22.
 */
function range(version: IetfVersion, start: Location, end: Location, trackName = "video"): FetchType {
	if (version === D18) return { kind: "standalone", trackNamespace: Path.from("room"), trackName, start, end };
	const endObject = end.object === 0n ? undefined : end.object - 1n;
	const filter: Filter.Filter = {
		kind: "absolute",
		startGroup: start.group,
		startObject: start.object,
		endGroup: end.group,
		endObject,
	};
	return filtered(filter, trackName);
}

function filtered(filter: Filter.Filter, trackName = "video"): FetchType {
	return { kind: "filtered", trackNamespace: Path.from("room"), trackName, filter };
}

function at(group: number, object: number): Location {
	return { group: BigInt(group), object: BigInt(object) };
}

test("FETCH and FETCH_OK round trip on every draft", async () => {
	const encode = async (msg: Fetch | FetchOk, version: IetfVersion) => {
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		await msg.encode(writer, version);
		writer.close();
		await writer.closed;
		return new Reader(undefined, Uint8Array.from(chunks.flatMap((chunk) => [...chunk])), version);
	};
	const types: [IetfVersion, FetchType][] = [
		[D18, range(D18, at(1, 2), at(3, 0))],
		[D18, { kind: "relativeJoining", subscription: 4n, offset: 1n }],
		[D18, { kind: "absoluteJoining", subscription: 4n, group: 300n }],
		[D22, filtered({ kind: "nextObject" })],
		[D22, range(D22, at(1, 2), at(3, 5))],
	];
	for (const [version, type] of types) {
		const msg = new Fetch({
			requestId: FETCH_ID,
			type,
			subscriberPriority: 3,
			groupOrder: "descending",
			fillTimeout: 250,
		});
		const decoded = await Fetch.decode(await encode(msg, version), version);
		expect(decoded).toEqual(msg);

		const ok = new FetchOk({ endOfTrack: true, endLocation: at(5, 2), properties: { timescale: Timescale.MILLI } });
		expect(await FetchOk.decode(await encode(ok, version), version)).toEqual(ok);
	}
	// Draft-22 has no Fetch Type tag, so the tagged forms cannot be written there, nor the
	// filtered one before it.
	await expect(encode(new Fetch({ requestId: 1n, type: range(D18, at(0, 0), at(0, 0)) }), D22)).rejects.toThrow();
	await expect(encode(new Fetch({ requestId: 1n, type: filtered({ kind: "unfiltered" }) }), D18)).rejects.toThrow();
});
