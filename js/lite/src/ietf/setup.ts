import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

const MAX_VERSIONS = 128;

export class ClientSetup {
	static id = 0x20;

	versions: number[];
	parameters: Parameters;

	constructor(versions: number[], parameters = new Parameters()) {
		this.versions = versions;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_15) {
			// Draft15: no versions list, just parameters
			// Make sure versions is draft 15 only.
			if (this.versions.length !== 1 || this.versions[0] !== Version.DRAFT_15) {
				throw new Error("versions must be draft 15 only");
			}

			await this.parameters.encode(w);
		} else if (version === Version.DRAFT_14) {
			await w.u53(this.versions.length);
			for (const v of this.versions) {
				await w.u53(v);
			}
			await this.parameters.encode(w);
		} else {
			const _: never = version;
			throw new Error(`unsupported version: ${_}`);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<ClientSetup> {
		if (version === Version.DRAFT_15) {
			// Draft15: no versions list, just parameters
			const parameters = await Parameters.decode(r);
			return new ClientSetup([Version.DRAFT_15], parameters);
		} else if (version === Version.DRAFT_14) {
			// Number of supported versions
			const numVersions = await r.u53();
			if (numVersions > MAX_VERSIONS) {
				throw new Error(`too many versions: ${numVersions}`);
			}

			const supportedVersions: number[] = [];

			for (let i = 0; i < numVersions; i++) {
				const v = await r.u53();
				supportedVersions.push(v);
			}

			const parameters = await Parameters.decode(r);

			return new ClientSetup(supportedVersions, parameters);
		} else {
			const _: never = version;
			throw new Error(`unsupported version: ${_}`);
		}
	}

	static async decode(r: Reader, version: IetfVersion): Promise<ClientSetup> {
		return Message.decode(r, (mr) => ClientSetup.#decode(mr, version));
	}
}

export class ServerSetup {
	static id = 0x21;

	version: number;
	parameters: Parameters;

	constructor(version: number, parameters = new Parameters()) {
		this.version = version;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_15) {
			// Draft15: no version field, just parameters
			await this.parameters.encode(w);
		} else if (version === Version.DRAFT_14) {
			await w.u53(this.version);
			await this.parameters.encode(w);
		} else {
			const _: never = version;
			throw new Error(`unsupported version: ${_}`);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<ServerSetup> {
		if (version === Version.DRAFT_15) {
			// Draft15: no version field, just parameters
			const parameters = await Parameters.decode(r);
			return new ServerSetup(Version.DRAFT_15, parameters);
		} else if (version === Version.DRAFT_14) {
			const selectedVersion = await r.u53();
			const parameters = await Parameters.decode(r);
			return new ServerSetup(selectedVersion, parameters);
		} else {
			const _: never = version;
			throw new Error(`unsupported version: ${_}`);
		}
	}

	static async decode(r: Reader, version: IetfVersion): Promise<ServerSetup> {
		return Message.decode(r, (mr) => ServerSetup.#decode(mr, version));
	}
}
