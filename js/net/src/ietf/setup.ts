import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

// Draft-17 unified SETUP message (0x2F00)
export class Setup {
	static id = 0x2f00;

	parameters: SetupOptions;

	constructor({ parameters = new SetupOptions() }: { parameters?: SetupOptions } = {}) {
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await this.parameters.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Setup> {
		const parameters = await SetupOptions.decode(r, version);
		return new Setup({ parameters });
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Setup> {
		return Message.decode(r, (mr) => Setup.#decode(mr, version));
	}
}

const MAX_VERSIONS = 128;

export class ClientSetup {
	static id = 0x20;

	versions: number[];
	parameters: SetupOptions;

	constructor({ versions, parameters = new SetupOptions() }: { versions: number[]; parameters?: SetupOptions }) {
		this.versions = versions;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// Draft15+: no versions list, just parameters
			await this.parameters.encode(w, version);
		} else if (version === Version.DRAFT_14) {
			await w.u53(this.versions.length);
			for (const v of this.versions) {
				await w.u53(v);
			}
			await this.parameters.encode(w, version);
		} else {
			// d17 uses unified Setup, not ClientSetup
			throw new Error("ClientSetup not used for this version");
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<ClientSetup> {
		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// Draft15+: no versions list, just parameters
			const parameters = await SetupOptions.decode(r, version);
			return new ClientSetup({ versions: [version], parameters });
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

			const parameters = await SetupOptions.decode(r, version);

			return new ClientSetup({ versions: supportedVersions, parameters });
		} else {
			// d17 uses unified Setup, not ClientSetup
			throw new Error("ClientSetup not used for this version");
		}
	}

	static async decode(r: Reader, version: IetfVersion): Promise<ClientSetup> {
		return Message.decode(r, (mr) => ClientSetup.#decode(mr, version));
	}
}

export class ServerSetup {
	static id = 0x21;

	version: number;
	parameters: SetupOptions;

	constructor({ version, parameters = new SetupOptions() }: { version: number; parameters?: SetupOptions }) {
		this.version = version;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// Draft15+: no version field, just parameters
			await this.parameters.encode(w, version);
		} else if (version === Version.DRAFT_14) {
			await w.u53(this.version);
			await this.parameters.encode(w, version);
		} else {
			// d17 uses unified Setup, not ServerSetup
			throw new Error("ServerSetup not used for this version");
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<ServerSetup> {
		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// Draft15+: no version field, just parameters
			const parameters = await SetupOptions.decode(r, version);
			return new ServerSetup({ version, parameters });
		} else if (version === Version.DRAFT_14) {
			const selectedVersion = await r.u53();
			const parameters = await SetupOptions.decode(r, version);
			return new ServerSetup({ version: selectedVersion, parameters });
		} else {
			// d17 uses unified Setup, not ServerSetup
			throw new Error("ServerSetup not used for this version");
		}
	}

	static async decode(r: Reader, version: IetfVersion): Promise<ServerSetup> {
		return Message.decode(r, (mr) => ServerSetup.#decode(mr, version));
	}
}
