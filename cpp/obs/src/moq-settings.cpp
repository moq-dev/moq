// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-settings.h"
#include "moq-describe.h"
#include "logger.h"

#include <algorithm>
#include <cstring>
#include <string>
#include <vector>

namespace MoQSettings {

namespace {

// Keys. These are stable: they land in scene collections and in the dock's settings
// file, so renaming one silently drops whatever the user had configured.
constexpr const char *BIND = "bind";

constexpr const char *TLS_DISABLE_VERIFY = "tls_disable_verify";
constexpr const char *TLS_FINGERPRINT = "tls_fingerprint";
constexpr const char *TLS_ROOT = "tls_root";

constexpr const char *BACKOFF_INITIAL = "backoff_initial_ms";
constexpr const char *BACKOFF_MAX = "backoff_max_ms";
constexpr const char *BACKOFF_TIMEOUT = "backoff_timeout_ms";

constexpr const char *QUIC_MAX_STREAMS = "quic_max_streams";

constexpr const char *WEBSOCKET_ENABLED = "websocket_enabled";
constexpr const char *WEBSOCKET_DELAY = "websocket_delay_ms";

// moq-ffi documents these defaults on the MoqClient setters but does not report them,
// so they are repeated here. The backoff defaults come from moq::Backoff itself.
constexpr long long QUIC_MAX_STREAMS_DEFAULT = 1024;
constexpr bool WEBSOCKET_ENABLED_DEFAULT = true;
constexpr long long WEBSOCKET_DELAY_DEFAULT_MS = 200;

// Read a string setting, returning nullptr when it's unset or empty, which leaves the
// knob at the library default.
const char *OptionalString(obs_data_t *settings, const char *key)
{
	const char *value = obs_data_get_string(settings, key);
	return (value && *value) ? value : nullptr;
}

// Read an integer setting inside the range declared by its Field. Scene collections
// are editable JSON, so the widget bounds alone do not protect the unsigned API.
uint64_t Amount(obs_data_t *settings, const char *key)
{
	const long long value = obs_data_get_int(settings, key);
	for (const Field &field : Fields()) {
		if (strcmp(field.key, key) == 0)
			return static_cast<uint64_t>(std::clamp(value, field.min, field.max));
	}

	LOG_ERROR("Advanced integer setting has no Field: %s", key);
	return 0;
}

// Shorthands so the table below reads as data rather than aggregate initializers.
Field Toggle(const char *key, const char *label, bool value, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Bool, 0, 0, 0, value, 0, "", {}, false, nullptr};
}

Field Number(const char *key, const char *label, long long value, long long min, long long max, long long step,
	     const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Int, min, max, step, false, value, "", {}, false, nullptr};
}

Field Text(const char *key, const char *label, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Text, 0, 0, 0, false, 0, "", {}, false, nullptr};
}

Field File(const char *key, const char *label, const char *filter, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::File, 0, 0, 0, false, 0, "", {}, false, filter};
}

long long Millis(uint64_t us)
{
	return static_cast<long long>(us / 1000);
}

} // namespace

const std::vector<Field> &Fields()
{
	static const std::vector<Field> fields = [] {
		const moq::Backoff backoff{};
		std::vector<Field> f;

		f.push_back(Text(BIND, "Bind address",
				 "Local UDP address to send from, e.g. 192.0.2.7:0 to pin the outgoing "
				 "interface. Leave empty for any."));

		// TLS.
		f.push_back(Toggle(TLS_DISABLE_VERIFY, "Skip certificate verification", false,
				   "Development only: accepts any certificate, so it cannot be combined "
				   "with a fingerprint or a root below. To trust one known self-signed "
				   "relay, turn this off and pin its fingerprint instead."));
		f.push_back(Text(TLS_FINGERPRINT, "Certificate fingerprint (SHA-256 hex)",
				 "Trust a self-signed certificate by pinning its fingerprint, without "
				 "accepting every certificate the way the option above does. Leave that "
				 "option off, or the stream refuses to start rather than quietly "
				 "ignoring this pin."));
		f.push_back(File(TLS_ROOT, "Root certificate (PEM)", "PEM (*.pem *.crt);;All Files (*)",
				 "Trust this CA instead of the system roots."));

		// Reconnect.
		f.push_back(Number(BACKOFF_INITIAL, "Reconnect delay (ms)", Millis(backoff.initial_us), 1, 60000, 100,
				   "Delay before the first reconnect attempt; it grows from here."));
		f.push_back(Number(BACKOFF_MAX, "Reconnect delay cap (ms)", Millis(backoff.max_us), 1, 600000, 1000,
				   "Ceiling on the growing reconnect delay."));
		f.push_back(Number(BACKOFF_TIMEOUT, "Give up after (ms)", Millis(backoff.timeout_us), 0, 3600000, 1000,
				   "Total time to keep retrying before the stream fails. Also how long the "
				   "broadcast lingers for viewers across the gap. 0 retries forever."));

		// QUIC transport.
		f.push_back(Number(QUIC_MAX_STREAMS, "Max concurrent streams", QUIC_MAX_STREAMS_DEFAULT, 1, 65536, 1,
				   "MoQ opens a stream per group, so a busy publisher wants this high."));

		// WebSocket fallback.
		f.push_back(Toggle(WEBSOCKET_ENABLED, "WebSocket fallback", WEBSOCKET_ENABLED_DEFAULT,
				   "Race a WebSocket connection against QUIC so a network that blocks UDP "
				   "still goes live. Turn it off to measure the QUIC path alone."));
		f.push_back(Number(WEBSOCKET_DELAY, "WebSocket fallback delay (ms)", WEBSOCKET_DELAY_DEFAULT_MS, 0,
				   10000, 50,
				   "How long QUIC gets a head start before the WebSocket attempt joins in."));

		return f;
	}();

	return fields;
}

void Defaults(obs_data_t *settings)
{
	obs_data_set_default_bool(settings, ENABLED, false);

	for (const Field &field : Fields()) {
		switch (field.kind) {
		case Kind::Bool:
			obs_data_set_default_bool(settings, field.key, field.bool_default);
			break;
		case Kind::Int:
			obs_data_set_default_int(settings, field.key, field.int_default);
			break;
		case Kind::Text:
		case Kind::File:
		case Kind::Directory:
		case Kind::Choice:
			obs_data_set_default_string(settings, field.key, field.text_default);
			break;
		}
	}
}

void AddProperties(obs_properties_t *props)
{
	obs_properties_t *group = obs_properties_create();

	for (const Field &field : Fields()) {
		obs_property_t *p = nullptr;

		switch (field.kind) {
		case Kind::Bool:
			p = obs_properties_add_bool(group, field.key, field.label);
			break;
		case Kind::Int:
			p = obs_properties_add_int(group, field.key, field.label, (int)field.min, (int)field.max,
						   (int)field.step);
			break;
		case Kind::Text:
			p = obs_properties_add_text(group, field.key, field.label, OBS_TEXT_DEFAULT);
			break;
		case Kind::File:
			p = obs_properties_add_path(group, field.key, field.label, OBS_PATH_FILE, field.filter,
						    nullptr);
			break;
		case Kind::Directory:
			p = obs_properties_add_path(group, field.key, field.label, OBS_PATH_DIRECTORY, nullptr,
						    nullptr);
			break;
		case Kind::Choice:
			p = obs_properties_add_list(group, field.key, field.label,
						    field.editable ? OBS_COMBO_TYPE_EDITABLE : OBS_COMBO_TYPE_LIST,
						    OBS_COMBO_FORMAT_STRING);
			for (const Option &option : field.options)
				obs_property_list_add_string(p, option.label, option.value);
			break;
		}

		if (p && field.tooltip)
			obs_property_set_long_description(p, field.tooltip);
	}

	obs_properties_add_group(props, ENABLED, "Advanced", OBS_GROUP_CHECKABLE, group);
}

bool Configure(obs_data_t *settings, moq::Client &client, std::string *error)
{
	if (!settings || !obs_data_get_bool(settings, ENABLED))
		return true; // advanced off: the client keeps the library defaults

	// Stops at the first setter moq-ffi rejects, naming the setting it came from.
	auto apply = [&](const char *key, moq::expected<void> result) {
		if (!result && error)
			*error = std::string(key) + ": " + MoQDescribe(result.error());
		return result.has_value();
	};

	if (const char *bind = OptionalString(settings, BIND)) {
		if (!apply(BIND, client.set_bind(bind)))
			return false;
	}

	if (obs_data_get_bool(settings, TLS_DISABLE_VERIFY)) {
		if (!apply(TLS_DISABLE_VERIFY, client.set_tls_verify(false)))
			return false;
	}
	if (const char *fingerprint = OptionalString(settings, TLS_FINGERPRINT)) {
		if (!apply(TLS_FINGERPRINT, client.set_tls_fingerprints({fingerprint})))
			return false;
	}
	if (const char *root = OptionalString(settings, TLS_ROOT)) {
		if (!apply(TLS_ROOT, client.set_tls_roots({root})))
			return false;
	}

	moq::Backoff backoff{};
	backoff.initial_us = Amount(settings, BACKOFF_INITIAL) * 1000;
	backoff.max_us = Amount(settings, BACKOFF_MAX) * 1000;
	backoff.timeout_us = Amount(settings, BACKOFF_TIMEOUT) * 1000;
	if (!apply(BACKOFF_INITIAL, client.set_backoff(backoff)))
		return false;

	if (!apply(QUIC_MAX_STREAMS, client.set_quic_max_streams(Amount(settings, QUIC_MAX_STREAMS))))
		return false;

	if (!apply(WEBSOCKET_ENABLED, client.set_websocket_enabled(obs_data_get_bool(settings, WEBSOCKET_ENABLED))))
		return false;
	if (!apply(WEBSOCKET_DELAY, client.set_websocket_delay(Amount(settings, WEBSOCKET_DELAY) * 1000)))
		return false;

	return true;
}

} // namespace MoQSettings
