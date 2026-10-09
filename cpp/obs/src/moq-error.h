// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <string>

namespace MoQError {

enum class Kind { Unauthorized, Forbidden, Certificate, Timeout, Network, Connect, Offline, Other };

// What the output recorded a failure as, beside its text. None means no failure.
enum Code : int { None = 0, Unauthorized, Forbidden, Connect, Offline, Other };

inline Kind Classify(int code, std::string reason)
{
	for (char &c : reason) {
		if (c >= 'A' && c <= 'Z')
			c = static_cast<char>(c - 'A' + 'a');
	}
	auto has = [&](const char *text) {
		return reason.find(text) != std::string::npos;
	};
	if (has("unauthorized") || code == Code::Unauthorized)
		return Kind::Unauthorized;
	if (has("forbidden") || code == Code::Forbidden)
		return Kind::Forbidden;
	// Fetching the pin happens before TLS verification and can fail on an outage.
	const bool fetching = has("failed to fetch fingerprint") || has("fingerprint request failed") ||
			      has("failed to read fingerprint") || has("failed to fetch certificate fingerprint");
	if (!fetching && (has("fingerprint") || has("certificate")))
		return Kind::Certificate;
	if (has("timed out") || has("timeout"))
		return Kind::Timeout;
	if (fetching || has("failed to connect") || has("dns"))
		return Kind::Network;
	if (code == Code::Connect || has("connect error"))
		return Kind::Connect;
	if (code == Code::Offline || reason == "offline")
		return Kind::Offline;
	return Kind::Other;
}

} // namespace MoQError
