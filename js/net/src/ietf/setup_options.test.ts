import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { implementationFromSetup, implementationIntoSetup, SetupOption, SetupOptions } from "./setup_options.ts";

// Encoder and decoder share these keys, so only a spec table catches a renumbering. The
// extension keys come from each draft-lcurley-moq-* IANA section; even keys are bare varints.
test("setup option keys match the registries", () => {
	expect({ ...SetupOption }).toEqual({
		Path: 0x01n,
		AuthorizationToken: 0x03n,
		MaxAuthTokenCacheSize: 0x04n,
		Authority: 0x05n,
		Implementation: 0x07n,
		HopId: 0x40b54n,
		RelayCost: 0x40b56n,
		Solicit: 0x40b5an,
		Hidden: 0x40b5cn,
		MaxAge: 0x915c4n,
		Auth: 0x40b70n,
		ActiveCount: 0x40b64n,
	});
});

test("IMPLEMENTATION is omitted when empty and refused when not UTF-8", () => {
	const empty = new SetupOptions();
	implementationIntoSetup(empty, "");
	expect(empty.size).toBe(0);
	expect(implementationFromSetup(empty)).toBeUndefined();

	const advertised = new SetupOptions();
	implementationIntoSetup(advertised, "moq-net-js/0.4.2");
	expect(implementationFromSetup(advertised)).toBe("moq-net-js/0.4.2");

	const malformed = new SetupOptions();
	malformed.setBytes(SetupOption.Implementation, new Uint8Array([0xc3, 0x28]));
	expect(() => implementationFromSetup(malformed)).toThrow(ProtocolViolation);
});
