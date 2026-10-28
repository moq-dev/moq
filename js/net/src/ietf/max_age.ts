/** Subscriber age budgets from the MoQ Timestamp extension. @module @internal */
import { SetupOption, type SetupOptions } from "./setup_options.ts";

/** The negotiated max-age Setup Option. @internal */
const MAX_AGE_OPTION = SetupOption.MaxAge;
/** The subscriber max-age message parameter. @internal */
export const MAX_AGE_PARAMETER = 0x915c6n;

/** Whether the peer accepts subscriber max age. @internal */
export function maxAgeFromSetup(params: SetupOptions): boolean {
	return params.getVarint(MAX_AGE_OPTION) !== undefined;
}

/** Declare support for subscriber max age. @internal */
export function maxAgeIntoSetup(params: SetupOptions): void {
	params.setVarint(MAX_AGE_OPTION, 1n);
}
