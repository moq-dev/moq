import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { SetupOptions } from "./setup_options.ts";
import type { IetfVersion } from "./version.ts";

// The SETUP message (0x2F00)
export class Setup {
	static id = 0x2f00;

	parameters: SetupOptions;

	constructor({ parameters = new SetupOptions() }: { parameters?: SetupOptions } = {}) {
		this.parameters = parameters;
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.parameters.encode(mw));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<Setup> {
		return Message.decode(r, async (mr) => new Setup({ parameters: await SetupOptions.decode(mr) }));
	}
}
