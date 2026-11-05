import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/** A request to list every track under a namespace prefix, which we refuse. @internal */
export class SubscribeTracks {
	static readonly id = 0x51;
	readonly requestId: bigint;
	readonly prefix: Path.Valid;
	readonly parameters: Parameters;
	constructor(requestId: bigint, prefix: Path.Valid, parameters = new Parameters()) {
		this.requestId = requestId;
		this.prefix = prefix;
		this.parameters = parameters;
	}

	async encode(writer: Writer, version: IetfVersion): Promise<void> {
		if (version < Version.DRAFT_18) throw new Error("SUBSCRIBE_TRACKS before draft-18");
		await Message.encode(writer, async (body) => {
			await body.u62(this.requestId);
			await Namespace.encode(body, this.prefix);
			await this.parameters.encode(body, version);
		});
	}

	static async decode(reader: Reader, version: IetfVersion): Promise<SubscribeTracks> {
		if (version < Version.DRAFT_18) throw new Error("SUBSCRIBE_TRACKS before draft-18");
		return Message.decode(reader, async (body) => {
			const requestId = await body.u62();
			const prefix = await Namespace.decode(body);
			const parameters = await Parameters.decode(body, version, "subscribe-tracks");
			if (parameters.fillParameters) Filter.decodeFill(parameters.fillParameters, version);
			return new SubscribeTracks(requestId, prefix, parameters);
		});
	}
}
