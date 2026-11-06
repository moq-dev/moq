import { concat, encodeVarint, Reader, Writer } from "../stream.ts";
import { StreamId } from "./stream.ts";
import { Version } from "./version.ts";

/** The most versions a CLIENT_SETUP may offer. */
const MAX_VERSIONS = 128;

/** An empty parameter list: moq-lite defines none here. */
const NO_PARAMETERS = new Uint8Array([0]);

/** Whether `version` is a moq-lite version, each of which negotiates over the `moql` SETUP exchange. */
export function isNegotiated(version: number): version is Version {
	return (Object.values(Version) as number[]).includes(version);
}

/** Write `id`, then the body `f` writes behind draft-14's u16 size, in one write. */
async function frame(w: Writer, id: number, f: (w: Writer) => Promise<void>): Promise<void> {
	const body = await Writer.collect(w.version, f);
	if (body.byteLength > 0xffff) throw new Error(`SETUP too large: ${body.byteLength} bytes`);
	await w.write(
		concat([encodeVarint(id, w.version), new Uint8Array([body.byteLength >> 8, body.byteLength & 0xff]), body]),
	);
}

/** Read the type `id`, then a u16-sized body, refusing anything else. */
async function unframe(r: Reader, id: number): Promise<Reader> {
	const kind = await r.u53();
	if (kind !== id) throw new Error(`unexpected SETUP message type: 0x${kind.toString(16)}`);
	const size = await r.u16();
	return new Reader(undefined, await r.read(size), r.version);
}

/**
 * The client half of the `moql` SETUP exchange, offering moq-lite versions.
 *
 * The framing is moq-transport draft-14's CLIENT_SETUP, which moq-lite borrowed: a u16
 * size, the offered versions, then parameters.
 */
export class ClientSetup {
	/** The offered versions, most preferred first. */
	versions: number[];

	constructor(versions: number[]) {
		this.versions = versions;
	}

	async encode(w: Writer): Promise<void> {
		await frame(w, StreamId.ClientCompat, async (w) => {
			await w.u53(this.versions.length);
			for (const v of this.versions) await w.u53(v);
			await w.write(NO_PARAMETERS);
		});
	}

	static async decode(r: Reader): Promise<ClientSetup> {
		const body = await unframe(r, StreamId.ClientCompat);
		const count = await body.u53();
		if (count > MAX_VERSIONS) throw new Error(`too many versions: ${count}`);
		const versions: number[] = [];
		for (let i = 0; i < count; i++) versions.push(await body.u53());
		// Parameters are unparsed: moq-lite defines none here.
		await body.readAll();
		return new ClientSetup(versions);
	}
}

/**
 * The server half of the `moql` SETUP exchange, selecting a moq-lite version.
 *
 * The framing is moq-transport draft-14's SERVER_SETUP: a u16 size, the selected version,
 * then parameters.
 */
export class ServerSetup {
	/** The selected version. */
	version: number;

	constructor(version: number) {
		this.version = version;
	}

	async encode(w: Writer): Promise<void> {
		await frame(w, StreamId.ServerCompat, async (w) => {
			await w.u53(this.version);
			await w.write(NO_PARAMETERS);
		});
	}

	static async decode(r: Reader): Promise<ServerSetup> {
		const body = await unframe(r, StreamId.ServerCompat);
		const version = await body.u53();
		// Parameters are unparsed: moq-lite defines none here.
		await body.readAll();
		return new ServerSetup(version);
	}
}
