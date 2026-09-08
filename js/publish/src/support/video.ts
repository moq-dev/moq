import * as Util from "@moq/hang/util";
import type { Codec } from "./index";

/** Whether the browser reliably distinguishes hardware from software encoding. */
export function hardwareReliable(): boolean {
	// Firefox before 143 reports software codecs as hardware-capable (Mozilla bug 1967793).
	const firefoxVersion = navigator.userAgent.match(/Firefox\/(\d+)/i)?.[1];
	return !Util.Hacks.isSafari && (firefoxVersion === undefined || Number(firefoxVersion) >= 143);
}

/** Probe the browser's software and hardware support for a video codec. */
export async function probe(codec: string): Promise<Codec> {
	const software = codec.startsWith("av01")
		? { supported: false }
		: await VideoEncoder.isConfigSupported({
				codec,
				width: 1280,
				height: 720,
				hardwareAcceleration: "prefer-software",
			});

	const hardware = await VideoEncoder.isConfigSupported({
		codec,
		width: 1280,
		height: 720,
		hardwareAcceleration: "prefer-hardware",
	});

	// Some browsers accept software codecs under "prefer-hardware".
	const unknown = !hardwareReliable() || hardware.config?.hardwareAcceleration !== "prefer-hardware";

	return {
		hardware: unknown ? undefined : hardware.supported === true,
		software: software.supported === true,
	};
}
