import { expect, test } from "bun:test";
import { probe } from "./video";

for (const version of [140, 142, 143, 152]) {
	test(`Firefox ${version} reports hardware support only when its probe is trustworthy`, async () => {
		const userAgent = Object.getOwnPropertyDescriptor(navigator, "userAgent");
		const encoder = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
		Object.defineProperty(navigator, "userAgent", {
			configurable: true,
			value: `Mozilla/5.0 Firefox/${version}.0`,
		});
		Object.defineProperty(globalThis, "VideoEncoder", {
			configurable: true,
			value: { isConfigSupported: async (config: VideoEncoderConfig) => ({ supported: true, config }) },
		});
		try {
			expect(await probe("vp09.00.10.08")).toEqual({
				software: true,
				hardware: version < 143 ? undefined : true,
			});
		} finally {
			if (userAgent) Object.defineProperty(navigator, "userAgent", userAgent);
			else Reflect.deleteProperty(navigator, "userAgent");
			if (encoder) Object.defineProperty(globalThis, "VideoEncoder", encoder);
			else Reflect.deleteProperty(globalThis, "VideoEncoder");
		}
	});
}

test("software-only AV1 is not advertised as usable encoding", async () => {
	const encoder = Object.getOwnPropertyDescriptor(globalThis, "VideoEncoder");
	Object.defineProperty(globalThis, "VideoEncoder", {
		configurable: true,
		value: {
			isConfigSupported: async (config: VideoEncoderConfig) => ({
				supported: config.hardwareAcceleration === "prefer-software",
				config,
			}),
		},
	});
	try {
		expect(await probe("av01.0.08M.08")).toEqual({ software: false, hardware: false });
	} finally {
		if (encoder) Object.defineProperty(globalThis, "VideoEncoder", encoder);
		else Reflect.deleteProperty(globalThis, "VideoEncoder");
	}
});
