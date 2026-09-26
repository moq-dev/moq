// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <moq/moq.hpp>

#include <string>
#include <type_traits>
#include <variant>

#include "moq-error.h"

// The text Rust gives a moq::Error, which is what the log and the dock show.
// TODO: use the message moq::Error carries once it has one (/quest/m1/cpp/error-message.md).
inline std::string MoQDescribe(const moq::Error &error)
{
	return std::visit(
		[](const auto &e) -> std::string {
			using E = std::decay_t<decltype(e)>;
			if constexpr (std::is_same_v<E, moq::Error::kProtocol>)
				return e.details.message;
			else if constexpr (std::is_same_v<E, moq::Error::kTransport>)
				return "transport: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kInternal>)
				return "internal: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kUrl>)
				return "url: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kLogLevel>)
				return "log level: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kTask>)
				return "task: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kJson>)
				return "json: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kConnect>)
				return "connect: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kBind>)
				return "bind: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kReject>)
				return "reject: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kCodec>)
				return "codec: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kTimeOverflow>)
				return "timestamp overflow";
			else if constexpr (std::is_same_v<E, moq::Error::kCancelled>)
				return "cancelled";
			else if constexpr (std::is_same_v<E, moq::Error::kClosed>)
				return "closed";
			else if constexpr (std::is_same_v<E, moq::Error::kBusy>)
				return "busy";
			else if constexpr (std::is_same_v<E, moq::Error::kAlreadyResponded>)
				return "already responded";
			else if constexpr (std::is_same_v<E, moq::Error::kUnauthorized>)
				return "unauthorized";
			else if constexpr (std::is_same_v<E, moq::Error::kForbidden>)
				return "forbidden";
			else if constexpr (std::is_same_v<E, moq::Error::kNotFound>)
				return "not found";
			else if constexpr (std::is_same_v<E, moq::Error::kUnsupported>)
				return "unsupported";
			else if constexpr (std::is_same_v<E, moq::Error::kAlreadyCommitted>)
				return "already committed to the other delivery order";
			else if constexpr (std::is_same_v<E, moq::Error::kInvalidRoute>)
				return "invalid route: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kInvalidPattern>)
				return "invalid pattern: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kUnresolvableBroadcast>)
				return "unresolvable broadcast reference: " + e.v1;
			else if constexpr (std::is_same_v<E, moq::Error::kLog>)
				return "log: " + e.v1;
			else
				return e.v1;
		},
		error.get_variant());
}

// The failure code the dock classifies a moq::Error by, beside its text.
inline MoQError::Code MoQFailureCode(const moq::Error &error)
{
	const auto &variant = error.get_variant();
	if (std::holds_alternative<moq::Error::kUnauthorized>(variant))
		return MoQError::Code::Unauthorized;
	if (std::holds_alternative<moq::Error::kForbidden>(variant))
		return MoQError::Code::Forbidden;
	if (std::holds_alternative<moq::Error::kConnect>(variant))
		return MoQError::Code::Connect;
	if (const auto *protocol = std::get_if<moq::Error::kProtocol>(&variant)) {
		if (protocol->details.kind == moq::ProtocolKind::kUnauthorized)
			return MoQError::Code::Unauthorized;
	}
	return MoQError::Code::Other;
}
