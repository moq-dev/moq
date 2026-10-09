import { expect, mock, spyOn, test } from "bun:test";
import { File as FileSource } from "./file";

const flush = () => new Promise<void>((resolve) => queueMicrotask(resolve));

async function settle(times = 5): Promise<void> {
	for (let i = 0; i < times; i++) await flush();
}

test("the file picker opens before yielding to media loading", () => {
	const document = Object.getOwnPropertyDescriptor(globalThis, "document");
	const click = mock(() => {});
	const input = { type: "", accept: "", addEventListener: mock(() => {}), click };
	Object.defineProperty(globalThis, "document", {
		configurable: true,
		value: { createElement: () => input },
	});
	const source = new FileSource();
	try {
		source.prompt();
		expect(click).toHaveBeenCalledTimes(1);
		expect(input.type).toBe("file");
		expect(input.accept).toBe("image/*,video/*,audio/*");
	} finally {
		source.close();
		if (document) Object.defineProperty(globalThis, "document", document);
		else Reflect.deleteProperty(globalThis, "document");
	}
});

test.each(["close", "clear", "disable"])("%s during media import does not start reading the file", async (action) => {
	const { Input } = await import("mediabunny");
	const canRead = spyOn(Input.prototype, "canRead").mockResolvedValue(false);
	const { Signal } = await import("@moq/signals");
	const enabled = new Signal(true);
	const source = new FileSource({
		file: new File([new Uint8Array([0])], "video.mp4", { type: "video/mp4" }),
		enabled,
	});
	// The effect starts decoding in its first microtask. Teardown runs before import resumes.
	queueMicrotask(() => {
		if (action === "close") source.close();
		else if (action === "clear") source.file.set(undefined);
		else enabled.set(false);
	});
	try {
		await settle(20);
		expect(canRead).not.toHaveBeenCalled();
		expect(source.out.source.peek()).toBeUndefined();
	} finally {
		source.close();
		canRead.mockRestore();
	}
});

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
