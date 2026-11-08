import { VERSION } from "./version.ts";

// This library's name and version as a User-Agent style product token, the default each side
// advertises in SETUP. Run from source rather than a published build, it reads `moq-net-js/dev`.
export const IMPLEMENTATION = `moq-net-js/${VERSION}`;
