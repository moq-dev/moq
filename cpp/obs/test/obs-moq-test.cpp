// SPDX-License-Identifier: GPL-2.0-or-later
//
// Loads and unloads the real module entry points against stubbed libobs, and
// checks that unloading stops moq-ffi. OBS unmaps the module after
// obs_module_unload returns, so a runtime or dispatcher thread still running in
// it would crash the process on exit.
//
// Run with `just obs test` (ThreadSanitizer) or `just obs ci` (plain). This is not
// part of the plugin build.
#include <chrono>
#include <cstdio>
#include <future>

#include <obs-module.h>

#include "moq-test-relay.h"

extern "C" {

void blog(int, const char *, ...) {}

lookup_t *obs_module_load_locale(obs_module_t *, const char *, const char *)
{
	return nullptr;
}

void text_lookup_destroy(lookup_t *) {}

bool text_lookup_getstr(lookup_t *, const char *, const char **)
{
	return false;
}

} // extern "C"

void register_moq_output() {}
void register_moq_service() {}
void register_moq_source() {}

int main()
{
	if (!obs_module_load()) {
		fprintf(stderr, "FAIL: obs_module_load\n");
		return 1;
	}

	// A call parked on the runtime, which only a shutdown can end without a peer.
	auto server = moq::Server::init();
	TestOk(server->set_bind("127.0.0.1:0"), "set_bind");
	TestOk(server->set_tls_generate({"localhost"}), "set_tls_generate");
	TestOk(server->listen().get(), "listen");
	auto pending = server->accept();
	if (pending.wait_for(std::chrono::milliseconds(50)) != std::future_status::timeout) {
		fprintf(stderr, "FAIL: accept resolved with no peer\n");
		return 1;
	}

	obs_module_unload();

	// Shutdown drops every task on the runtime, so the parked call ends instead of
	// waiting on a thread that is gone.
	if (pending.wait_for(std::chrono::seconds(5)) != std::future_status::ready) {
		fprintf(stderr, "FAIL: obs_module_unload left the moq-ffi runtime running\n");
		return 1;
	}

	printf("module unload stops the moq-ffi runtime: ok\n");
	return 0;
}
