// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <string>

namespace MoQDial {

inline bool IsCleartext(std::string scheme)
{
	for (char &c : scheme) {
		if (c >= 'A' && c <= 'Z')
			c = static_cast<char>(c - 'A' + 'a');
	}
	return scheme == "http" || scheme == "ws" || scheme == "tcp";
}

} // namespace MoQDial
