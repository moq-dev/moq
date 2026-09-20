import { expect, test } from "bun:test";
import { File as FileSource } from "./file";

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));

async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("clearing a decoded file clears its published media", async () => {
	const createImageBitmap = Object.getOwnPropertyDescriptor(globalThis, "createImageBitmap");
	const videoFrame = Object.getOwnPropertyDescriptor(globalThis, "VideoFrame");

	Object.defineProperty(globalThis, "createImageBitmap", {
		configurable: true,
		value: async () => ({ close() {} }),
	});
	Object.defineProperty(globalThis, "VideoFrame", {
		configurable: true,
		value: class {
			close() {}
		},
	});

	const source = new FileSource({
		file: new File([new Uint8Array([0])], "still.png", { type: "image/png" }),
	});

	try {
		await settle();
		expect(source.out.source.peek()?.video).toBeDefined();

		source.file.set(undefined);
		await settle();
		expect(source.out.source.peek()).toBeUndefined();
	} finally {
		source.close();
		if (createImageBitmap) Object.defineProperty(globalThis, "createImageBitmap", createImageBitmap);
		else Reflect.deleteProperty(globalThis, "createImageBitmap");
		if (videoFrame) Object.defineProperty(globalThis, "VideoFrame", videoFrame);
		else Reflect.deleteProperty(globalThis, "VideoFrame");
	}
});
