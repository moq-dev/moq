import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class Extensions {
	entries: Map<bigint, Uint8Array>;

	constructor() {
		this.entries = new Map();
	}

	set(id: bigint, value: Uint8Array) {
		this.entries.set(id, value);
	}

	get(id: bigint): Uint8Array | undefined {
		return this.entries.get(id);
	}

	remove(id: bigint): Uint8Array | undefined {
		const value = this.entries.get(id);
		this.entries.delete(id);
		return value;
	}

	async encode(w: Writer) {
		await w.u53(this.entries.size);
		for (const [id, value] of this.entries) {
			await w.u62(id);
			await w.u53(value.length);
			await w.write(value);
		}
	}

	static async decode(r: Reader): Promise<Extensions> {
		const count = await r.u53();
		const params = new Extensions();

		for (let i = 0; i < count; i++) {
			const id = await r.u62();
			const size = await r.u53();
			const value = await r.read(size);

			if (params.entries.has(id)) {
				throw new Error(`duplicate parameter id: ${id.toString()}`);
			}

			params.entries.set(id, value);
		}

		return params;
	}
}

export class SessionClient {
	versions: number[];
	extensions: Extensions;

	constructor(versions: number[], extensions = new Extensions()) {
		this.versions = versions;
		this.extensions = extensions;
	}

	async #encode(w: Writer) {
		await w.u53(this.versions.length);
		for (const v of this.versions) {
			await w.u53(v);
		}

		await this.extensions.encode(w);
	}

	static async #decode(r: Reader): Promise<SessionClient> {
		const versions: number[] = [];
		const count = await r.u53();
		for (let i = 0; i < count; i++) {
			versions.push(await r.u53());
		}

		const extensions = await Extensions.decode(r);
		return new SessionClient(versions, extensions);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<SessionClient> {
		return Message.decode(r, SessionClient.#decode);
	}
}

export class SessionServer {
	version: number;
	extensions: Extensions;

	constructor(version: number, extensions = new Extensions()) {
		this.version = version;
		this.extensions = extensions;
	}

	async #encode(w: Writer) {
		await w.u53(this.version);
		await this.extensions.encode(w);
	}

	static async #decode(r: Reader): Promise<SessionServer> {
		const version = await r.u53();
		const extensions = await Extensions.decode(r);
		return new SessionServer(version, extensions);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<SessionServer> {
		return Message.decode(r, SessionServer.#decode);
	}
}

export class SessionInfo {
	bitrate: number;

	constructor(bitrate: number) {
		this.bitrate = bitrate;
	}

	static #guard(version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			case Version.DRAFT_03:
				throw new Error("session info not supported for Draft03");
			default:
				unreachable(version);
		}
	}

	async #encode(w: Writer) {
		await w.u53(this.bitrate);
	}

	static async #decode(r: Reader): Promise<SessionInfo> {
		const bitrate = await r.u53();
		return new SessionInfo(bitrate);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		SessionInfo.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<SessionInfo> {
		SessionInfo.#guard(version);
		return Message.decode(r, SessionInfo.#decode);
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<SessionInfo | undefined> {
		SessionInfo.#guard(version);
		return Message.decodeMaybe(r, SessionInfo.#decode);
	}
}
