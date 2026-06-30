import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { hasSetupStream, type Version } from "./version.ts";

/// Setup parameter ID for the request Path (client-only, on URI-less transports).
const PARAM_PATH = 0x2;

/**
 * The moq-lite-05 SETUP message, sent once per endpoint on a unidirectional Setup stream.
 *
 * The browser conveys its path via the WebTransport URL, so it never sends a Path
 * parameter; this class still parses one for completeness when reading a peer's SETUP.
 */
export class Setup {
	/// The request path, only sent on transport bindings without a request URI.
	path?: string;

	constructor(props: { path?: string } = {}) {
		this.path = props.path;
	}

	async #encode(w: Writer) {
		if (this.path !== undefined) {
			await w.u53(1); // parameter count
			const value = new TextEncoder().encode(this.path);
			await w.u53(PARAM_PATH);
			await w.u53(value.byteLength);
			await w.write(value);
		} else {
			await w.u53(0); // no parameters
		}
	}

	static async #decode(r: Reader): Promise<Setup> {
		const count = await r.u53();
		let path: string | undefined;
		for (let i = 0; i < count; i++) {
			const id = await r.u53();
			const len = await r.u53();
			const value = await r.read(len);
			// Unknown parameters are ignored so new ones stay backward compatible.
			if (id === PARAM_PATH) {
				path = new TextDecoder().decode(value);
			}
		}
		return new Setup({ path });
	}

	async encode(w: Writer, version: Version): Promise<void> {
		if (!hasSetupStream(version)) throw new Error("SETUP requires moq-lite-05+");
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<Setup> {
		if (!hasSetupStream(version)) throw new Error("SETUP requires moq-lite-05+");
		return Message.decode(r, Setup.#decode);
	}
}
