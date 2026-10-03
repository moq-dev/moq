import * as Moq from "@moq/net";

/** Pin the transport to one wire version, including drafts omitted from defaults. */
export async function connect(props: Moq.Connection.ConnectProps) {
	const version = process.env.INTEROP_VERSION;
	if (!version) return Moq.Connection.connect(props);
	const url = new URL(props.url);
	const fingerprintUrl = new URL("/certificate.sha256", url);
	const response = await fetch(fingerprintUrl);
	if (!response.ok) throw new Error(`certificate endpoint: ${response.status}`);
	const hash = (await response.text()).trim();
	if (!/^[a-f0-9]{64}$/i.test(hash)) throw new Error("malformed certificate fingerprint");
	url.protocol = "https:";
	// These three drafts share moql and select the precise version in SETUP.
	const protocol = /^(moq-lite-0[12]|moq-transport-14)$/.test(version) ? "moql" : version;
	const transport = new WebTransport(url, {
		protocols: [protocol],
		serverCertificateHashes: [{ algorithm: "sha-256", value: Uint8Array.from(Buffer.from(hash, "hex")) }],
	});
	await transport.ready;
	const connection = await Moq.Connection.connect({ ...props, transport });
	if (connection.version !== version) {
		connection.close();
		throw new Error(`requested ${version}, negotiated ${connection.version}`);
	}
	return connection;
}
