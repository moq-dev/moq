// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-dial.h"

#include <cassert>
#include <cstdio>

int main()
{
	for (const char *scheme : {"http", "ws", "tcp", "TCP", "Http", "Ws"})
		assert(MoQDial::IsCleartext(scheme));
	for (const char *scheme : {"https", "wss", "moqt", "moql", "HTTPS"})
		assert(!MoQDial::IsCleartext(scheme));
	std::puts("publish tokens reject cleartext network schemes: ok");
}
