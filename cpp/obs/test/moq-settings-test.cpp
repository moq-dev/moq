// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the advanced settings against stubbed obs_data and the real moq-ffi: the
// defaults come from the library, and every stored key lands on its config field, so a
// scene collection written before the C++ migration applies again.
//
// Run with `just obs test` (ThreadSanitizer) or `just obs ci` (plain). This is not
// part of the plugin build.
#include <chrono>
#include <cstdio>
#include <map>
#include <string>

#include <obs-module.h>

#include "moq-settings.h"

// A flat key-value store standing in for obs_data: a set value wins over a default.
struct obs_data {
	std::map<std::string, std::string> strings;
	std::map<std::string, long long> ints;
	std::map<std::string, bool> bools;
	std::map<std::string, std::string> default_strings;
	std::map<std::string, long long> default_ints;
	std::map<std::string, bool> default_bools;
};

namespace {

template<typename T>
T Lookup(const std::map<std::string, T> &set, const std::map<std::string, T> &defaults, const char *key, T fallback)
{
	if (auto it = set.find(key); it != set.end())
		return it->second;
	if (auto it = defaults.find(key); it != defaults.end())
		return it->second;
	return fallback;
}

} // namespace

extern "C" {

void blog(int, const char *, ...) {}

const char *obs_data_get_string(obs_data_t *data, const char *name)
{
	if (auto it = data->strings.find(name); it != data->strings.end())
		return it->second.c_str();
	if (auto it = data->default_strings.find(name); it != data->default_strings.end())
		return it->second.c_str();
	return "";
}

long long obs_data_get_int(obs_data_t *data, const char *name)
{
	return Lookup(data->ints, data->default_ints, name, 0LL);
}

bool obs_data_get_bool(obs_data_t *data, const char *name)
{
	return Lookup(data->bools, data->default_bools, name, false);
}

void obs_data_set_default_string(obs_data_t *data, const char *name, const char *val)
{
	data->default_strings[name] = val;
}

void obs_data_set_default_int(obs_data_t *data, const char *name, long long val)
{
	data->default_ints[name] = val;
}

void obs_data_set_default_bool(obs_data_t *data, const char *name, bool val)
{
	data->default_bools[name] = val;
}

// AddProperties is linked in but not exercised here.
obs_properties_t *obs_properties_create(void)
{
	return nullptr;
}

obs_property_t *obs_properties_add_bool(obs_properties_t *, const char *, const char *)
{
	return nullptr;
}

obs_property_t *obs_properties_add_int(obs_properties_t *, const char *, const char *, int, int, int)
{
	return nullptr;
}

obs_property_t *obs_properties_add_text(obs_properties_t *, const char *, const char *, enum obs_text_type)
{
	return nullptr;
}

obs_property_t *obs_properties_add_path(obs_properties_t *, const char *, const char *, enum obs_path_type,
					const char *, const char *)
{
	return nullptr;
}

obs_property_t *obs_properties_add_list(obs_properties_t *, const char *, const char *, enum obs_combo_type,
					enum obs_combo_format)
{
	return nullptr;
}

size_t obs_property_list_add_string(obs_property_t *, const char *, const char *)
{
	return 0;
}

void obs_property_set_long_description(obs_property_t *, const char *) {}

obs_property_t *obs_properties_add_group(obs_properties_t *, const char *, const char *, enum obs_group_type,
					 obs_properties_t *)
{
	return nullptr;
}

} // extern "C"

namespace {
int g_failures = 0;

#define CHECK(cond)                                                                 \
	do {                                                                        \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                               \
		}                                                                   \
	} while (0)

} // namespace

// A library default as the whole milliseconds the settings table shows.
static long long Ms(std::chrono::nanoseconds duration)
{
	return std::chrono::duration_cast<std::chrono::milliseconds>(duration).count();
}

int main()
{
	const moq::ClientConfig library = moq::client_defaults();

	// The defaults a fresh scene collection gets are the library's, not repeated constants.
	{
		obs_data settings;
		MoQSettings::Defaults(&settings);
		CHECK(settings.default_ints["connect_timeout_ms"] == Ms(*library.timeout));
		CHECK(settings.default_ints["failover_delay_ms"] == Ms(*library.race));
		CHECK(settings.default_ints["quic_max_streams"] == (long long)*library.quic.max_streams);
		CHECK(settings.default_ints["quic_idle_timeout_ms"] == Ms(*library.quic.idle_timeout));
		CHECK(settings.default_ints["quic_keep_alive_ms"] == Ms(*library.quic.keep_alive));
		CHECK(settings.default_ints["backoff_timeout_ms"] == Ms(*library.backoff.timeout));
		CHECK(settings.default_bools["websocket_enabled"] == *library.websocket.enabled);
		CHECK(settings.default_strings["quic_gso"].empty());

		// Advanced off leaves the config untouched.
		moq::ClientConfig config;
		CHECK(MoQSettings::Configure(&settings, config).empty());
		CHECK(!config.timeout && !config.quic.max_streams);
	}
	printf("defaults come from moq-ffi: ok\n");

	// Every key restored from the pre-migration plugin reaches its config field.
	{
		const std::string version = moq::versions().front();
		obs_data settings;
		MoQSettings::Defaults(&settings);
		settings.bools["advanced"] = true;
		settings.strings["version"] = version;
		settings.ints["connect_timeout_ms"] = 7000;
		settings.ints["failover_delay_ms"] = 80;
		settings.strings["tls_host_name"] = "relay.example";
		settings.strings["quic_congestion_control"] = "loss";
		settings.ints["quic_idle_timeout_ms"] = 9000;
		settings.ints["quic_keep_alive_ms"] = 0;
		settings.strings["quic_gso"] = "off";
		settings.strings["quic_mtu_discovery"] = "on";

		moq::ClientConfig config;
		CHECK(MoQSettings::Configure(&settings, config).empty());
		CHECK(config.versions == std::vector<std::string>{version});
		CHECK(config.timeout == std::chrono::milliseconds(7000));
		CHECK(config.race == std::chrono::milliseconds(80));
		CHECK(config.tls.host_name == std::string("relay.example"));
		CHECK(config.quic.congestion_control == moq::CongestionControl::kLoss);
		CHECK(config.quic.idle_timeout == std::chrono::milliseconds(9000));
		CHECK(config.quic.keep_alive == std::chrono::nanoseconds::zero());
		CHECK(config.quic.gso == false);
		CHECK(config.quic.mtu_discovery == true);
		CHECK(!config.quic.qlog);
		CHECK(config.quic.max_streams == library.quic.max_streams);

		// moq-ffi accepts what the settings produced.
		CHECK(moq::Client::init(config).has_value());
	}
	printf("restored keys apply: ok\n");

	// The TLS key log is off unless the toggle asks for it.
	{
		obs_data settings;
		MoQSettings::Defaults(&settings);
		settings.bools["advanced"] = true;
		CHECK(settings.default_bools["quic_keylog"] == false);

		moq::ClientConfig config;
		CHECK(MoQSettings::Configure(&settings, config).empty());
		CHECK(config.quic.keylog != true);

		settings.bools["quic_keylog"] = true;
		CHECK(MoQSettings::Configure(&settings, config).empty());
		CHECK(config.quic.keylog == true);
	}
	printf("key log is opt-in: ok\n");

	// A hand-edited scene collection naming an unknown family refuses rather than ignores it.
	{
		obs_data settings;
		MoQSettings::Defaults(&settings);
		settings.bools["advanced"] = true;
		settings.strings["quic_congestion_control"] = "fast";
		moq::ClientConfig config;
		CHECK(MoQSettings::Configure(&settings, config).find("congestion") != std::string::npos);
	}
	printf("unknown congestion control refused: ok\n");

	// An unknown tri-state refuses rather than reading as disabled.
	{
		obs_data settings;
		MoQSettings::Defaults(&settings);
		settings.bools["advanced"] = true;
		settings.strings["quic_gso"] = "yes";
		moq::ClientConfig config;
		CHECK(MoQSettings::Configure(&settings, config) == "unknown quic_gso: yes");
	}
	printf("unknown tri-state refused: ok\n");

	// An out-of-range integer refuses rather than clamping: a negative connect timeout
	// would otherwise become 0, which waits forever.
	{
		obs_data settings;
		MoQSettings::Defaults(&settings);
		settings.bools["advanced"] = true;
		settings.ints["connect_timeout_ms"] = -1;
		moq::ClientConfig config;
		CHECK(MoQSettings::Configure(&settings, config) == "connect_timeout_ms out of range: -1");
	}
	printf("out-of-range integer refused: ok\n");

	moq::shutdown();
	return g_failures == 0 ? 0 : 1;
}
