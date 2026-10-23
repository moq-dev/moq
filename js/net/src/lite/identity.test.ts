import { expect, test } from "bun:test";
import { runIdentityCases, type Vectors } from "./identity.fixture";

test("unactivated identity proposal has Rust/WebCrypto parity and rejects adversarial proof/control mutations", async () => {
	const vectors: Vectors = await Bun.file(
		new URL("../../../../test/interop/identity/vectors.json", import.meta.url),
	).json();
	expect(await runIdentityCases(vectors)).toHaveLength(7);
});
