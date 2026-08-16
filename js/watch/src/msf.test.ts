import { expect, test } from "bun:test";
import type * as Msf from "@moq/msf";
import { toHang } from "./msf";

test("preserves stalled video renditions", () => {
	const catalog: Msf.Catalog = {
		tracks: [
			{
				name: "video",
				packaging: "loc",
				role: "video",
				codec: "vp09.00.10.08",
				stalled: true,
			},
		],
	};

	expect(toHang(catalog).video?.renditions.video?.stalled).toBe(true);
});
