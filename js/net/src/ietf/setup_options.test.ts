import { expect, test } from "bun:test";
import { SetupOption } from "./setup_options.ts";

// Encoder and decoder share these keys, so only a spec table catches a renumbering. The
// extension keys come from each draft-lcurley-moq-* IANA section; even keys are bare varints.
test("setup option keys match the registries", () => {
	expect({ ...SetupOption }).toEqual({
		Path: 0x01n,
		MaxRequestId: 0x02n,
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
		Epoch: 0x40b68n,
	});
});
