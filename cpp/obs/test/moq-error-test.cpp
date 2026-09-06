// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-error.h"

#include <cstdio>

int main()
{
	using MoQError::Classify;
	using MoQError::Kind;
	const char *outage = "connect error: reconnect timed out after 10s: failed to connect to server: "
			     "QUIC failed: failed to fetch fingerprint; WebSocket failed: failed to connect WebSocket";
	if (Classify(-5, outage) != Kind::Timeout || Classify(-5, "failed to fetch fingerprint") != Kind::Network ||
	    Classify(-5, "reconnect timed out after 10s: invalid peer certificate: UnknownIssuer") !=
		    Kind::Certificate ||
	    Classify(-34, "reconnect timed out after 10s") != Kind::Unauthorized ||
	    Classify(-35, "forbidden") != Kind::Forbidden || Classify(-17, "offline") != Kind::Offline) {
		std::fprintf(stderr, "FAIL: connection failure classification\n");
		return 1;
	}
	std::printf("network outage, certificate, auth, and offline classification: ok\n");
	return 0;
}
