// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <string>

// Relay URLs can embed credentials (userinfo) or a query/path token, and OBS
// logs are frequently shared for support. Reduce a URL to scheme://host[:port]
// for logging so secrets never reach persistent logs.
inline std::string MoQRedactUrl(const std::string &url)
{
	if (url.empty())
		return "(empty)";

	size_t scheme = url.find("://");
	std::string prefix = (scheme == std::string::npos) ? "" : url.substr(0, scheme + 3);
	size_t rest = (scheme == std::string::npos) ? 0 : scheme + 3;

	// The authority ends at the first '/', '?' or '#'.
	size_t auth_end = url.find_first_of("/?#", rest);
	std::string authority = url.substr(rest, auth_end == std::string::npos ? std::string::npos : auth_end - rest);

	// Drop any userinfo (user:pass@). Use the last '@' so an unescaped '@' in a
	// password can't leave part of it behind.
	size_t at = authority.rfind('@');
	if (at != std::string::npos)
		authority = authority.substr(at + 1);

	return prefix + authority;
}

inline std::string MoQRedactUrl(const char *url)
{
	if (!url || !*url)
		return "(null)";
	return MoQRedactUrl(std::string(url));
}
