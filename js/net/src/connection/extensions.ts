import * as Ietf from "../ietf/index.ts";

/**
 * The moq-transport extensions a session offers in its SETUP, each on unless set to `false`.
 * Turning one off connects as a peer that does not speak it. moq-lite carries both in its
 * core, so this only changes moq-transport sessions.
 */
export interface Extensions {
	/** The MoQ Auth extension: tokens presented and granted on their own stream (draft-17+). */
	auth?: boolean;
	/** The MoQ Solicit extension: the peer answers our SUBSCRIBE_NAMESPACE rather than announcing unasked. */
	solicit?: boolean;
}

/** Every extension resolved to whether it is offered. */
export type Offered = Required<Extensions>;

/** Resolve each unset extension to offered. */
export function offered(extensions?: Extensions): Offered {
	return { auth: extensions?.auth ?? true, solicit: extensions?.solicit ?? true };
}

/** Write the options for the offered extensions into a SETUP. */
export function intoSetup(params: Ietf.SetupOptions, extensions: Offered, version: Ietf.IetfVersion) {
	if (extensions.solicit) Ietf.solicitIntoSetup(params);
	if (extensions.auth) Ietf.Auth.intoSetup(params, version);
}
