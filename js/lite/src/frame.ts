import * as Time from "./time";

export class Frame {
	instant: Time.Milli;
	payload: Uint8Array;

	constructor({ payload, instant = Time.Milli.now() }: { payload: Uint8Array; instant?: Time.Milli }) {
		this.instant = instant;
		this.payload = payload;
	}

	static fromString(str: string, instant = Time.Milli.now()) {
		return new Frame({ payload: new TextEncoder().encode(str), instant });
	}

	static fromJson(json: unknown, instant = Time.Milli.now()) {
		return new Frame({ payload: new TextEncoder().encode(JSON.stringify(json)), instant });
	}

	static fromBool(bool: boolean, instant = Time.Milli.now()) {
		return new Frame({ payload: new Uint8Array([bool ? 1 : 0]), instant });
	}

	toString() {
		return new TextDecoder().decode(this.payload);
	}

	toJson() {
		return JSON.parse(this.toString());
	}

	toBool() {
		if (this.payload.byteLength !== 1) throw new Error("invalid bool frame");
		if (this.payload[0] === 0) return false;
		if (this.payload[0] === 1) return true;
		throw new Error("invalid bool frame");
	}
}
