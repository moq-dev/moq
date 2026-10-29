import { expect, test } from "bun:test";
import { diagnose } from "./diagnostics.ts";

test("page rejections retain binding fields, nested causes and non-JSON values", async () => {
	const binding = Object.assign(new Error("MoqError.Internal"), {
		inner: { reason: "the binding's failure reason", sequence: 1n << 63n },
	});
	const error = new Error("consume failed", { cause: binding });
	const detail = await diagnose(Promise.reject(error)).catch((caught: Error) => JSON.parse(caught.message));
	expect(detail.message).toBe("consume failed");
	expect(detail.stack).toContain("diagnostics.test.ts");
	expect(detail.cause.message).toBe("MoqError.Internal");
	expect(detail.cause.inner).toEqual({ reason: "the binding's failure reason", sequence: "9223372036854775808" });
});

test("circular and non-Error rejections remain diagnostic failures", async () => {
	const error = new Error("circular failure");
	error.cause = error;
	await expect(diagnose(Promise.reject(error))).rejects.toThrow('"cause":"[Circular]"');
	await expect(diagnose(Promise.reject(undefined))).rejects.toThrow("undefined");
	await expect(diagnose(Promise.reject("refused"))).rejects.toThrow("refused");
});
