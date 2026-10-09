// SPDX-License-Identifier: GPL-2.0-or-later
//
// A MoQ relay inside the test process, so the output and source tests drive the
// real moq-ffi over real QUIC instead of stubbing it. Header-only and included by
// one test per binary.
#pragma once

#include <moq/moq.hpp>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

// Aborts: the fixture can't carry on without its relay.
template<typename T> T TestOk(moq::expected<T> result, const char *what)
{
	if (!result) {
		std::fprintf(stderr, "FAIL: %s: moq::Error variant %zu\n", what, result.error().get_variant().index());
		std::abort();
	}
	return std::move(*result);
}

inline void TestOk(moq::expected<void> result, const char *what)
{
	if (!result) {
		std::fprintf(stderr, "FAIL: %s: moq::Error variant %zu\n", what, result.error().get_variant().index());
		std::abort();
	}
}

// Polls `done` until it holds or the timeout passes. For state the plugin only
// exposes by polling, like whether a session is live.
inline bool WaitFor(const std::function<bool()> &done, std::chrono::milliseconds timeout = std::chrono::seconds(10))
{
	const auto deadline = std::chrono::steady_clock::now() + timeout;
	while (!done()) {
		if (std::chrono::steady_clock::now() > deadline)
			return false;
		std::this_thread::sleep_for(std::chrono::milliseconds(2));
	}
	return true;
}

// A moq-ffi server that publishes every broadcast it consumes, so whatever one
// session announces, another can subscribe to.
//
// It also answers the plain-HTTP certificate request an http:// URL makes on the
// same port number, which is how a client that can't skip verification (the
// source dials with the defaults) trusts the generated certificate.
class TestRelay {
public:
	// With `accept` false the relay listens but never completes a handshake, so a
	// connect to it stays pending.
	explicit TestRelay(bool accept = true)
	{
		origin = moq::OriginProducer::init(moq::OriginConfig{});
		// The fingerprint needs the TCP port matching the UDP one the server drew, which
		// something else may hold; draw again until both are free.
		for (int attempt = 0; attempt < 20 && http_fd < 0; attempt++) {
			if (server)
				server->cancel();
			server = moq::Server::init();
			TestOk(server->set_bind("127.0.0.1:0"), "set_bind");
			TestOk(server->set_tls_generate({"localhost"}), "set_tls_generate");
			TestOk(server->set_publish(origin), "set_publish");
			TestOk(server->set_consume(origin), "set_consume");
			addr = TestOk(server->listen().get(), "listen");
			http_fd = BindFingerprint();
		}
		if (http_fd < 0) {
			std::perror("fingerprint server");
			std::abort();
		}
		fingerprint = TestOk(server->cert_fingerprints(), "cert_fingerprints").at(0);

		ServeFingerprint();
		if (accept)
			sessions_thread = std::thread([this] { AcceptLoop(); });
	}

	~TestRelay()
	{
		server->cancel();
		if (sessions_thread.joinable())
			sessions_thread.join();
		{
			std::lock_guard<std::mutex> lock(mutex);
			sessions.clear();
		}

		::shutdown(http_fd, SHUT_RDWR);
		::close(http_fd);
		http_thread.join();
	}

	TestRelay(const TestRelay &) = delete;
	TestRelay &operator=(const TestRelay &) = delete;

	// The relay's URL. http:// fetches the fingerprint; https:// needs verification off.
	std::string Url(const char *scheme = "http") const { return std::string(scheme) + "://" + addr; }

	// Close every accepted session, the way a relay restart would.
	void DropSessions()
	{
		std::lock_guard<std::mutex> lock(mutex);
		for (auto &session : sessions)
			session->cancel(0);
		sessions.clear();
	}

	// The session accepted `index`-th, or null before it was.
	std::shared_ptr<moq::Session> Session(size_t index)
	{
		std::lock_guard<std::mutex> lock(mutex);
		return index < sessions.size() ? sessions[index] : nullptr;
	}

	size_t Accepted()
	{
		std::lock_guard<std::mutex> lock(mutex);
		return accepted;
	}

	std::shared_ptr<moq::OriginProducer> origin;

private:
	void AcceptLoop()
	{
		for (;;) {
			auto request = server->accept().get();
			if (!request || !*request)
				return;
			auto session = (*request)->accept().get();
			if (!session)
				continue;
			std::lock_guard<std::mutex> lock(mutex);
			sessions.push_back(*session);
			accepted++;
		}
	}

	// A TCP listener on the server's port number, or -1 when that port is taken.
	int BindFingerprint() const
	{
		const auto colon = addr.rfind(':');
		const int port = std::stoi(addr.substr(colon + 1));

		const int fd = ::socket(AF_INET, SOCK_STREAM, 0);
		int on = 1;
		::setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof(on));
		sockaddr_in sin{};
		sin.sin_family = AF_INET;
		sin.sin_port = htons(static_cast<uint16_t>(port));
		sin.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
		if (::bind(fd, reinterpret_cast<sockaddr *>(&sin), sizeof(sin)) != 0 || ::listen(fd, 16) != 0) {
			::close(fd);
			return -1;
		}
		return fd;
	}

	void ServeFingerprint()
	{
		http_thread = std::thread([this] {
			for (;;) {
				const int client = ::accept(http_fd, nullptr, nullptr);
				if (client < 0)
					return;
				char request[1024];
				(void)::recv(client, request, sizeof(request), 0);
				const std::string response =
					"HTTP/1.1 200 OK\r\nContent-Length: " + std::to_string(fingerprint.size()) +
					"\r\nConnection: close\r\n\r\n" + fingerprint;
				(void)::send(client, response.data(), response.size(), MSG_NOSIGNAL);
				::close(client);
			}
		});
	}

	std::shared_ptr<moq::Server> server;
	std::string addr;
	std::string fingerprint;

	std::mutex mutex;
	std::vector<std::shared_ptr<moq::Session>> sessions;
	size_t accepted = 0;
	std::thread sessions_thread;

	int http_fd = -1;
	std::thread http_thread;
};

// An H.264 SPS and PPS (Annex-B, 1280x720 High), which is all the importer parses
// out of a keyframe; the slice after it is never decoded.
inline std::vector<uint8_t> TestH264Init()
{
	return {0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40,
		0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x0c, 0x23,
		0xc6, 0x0c, 0x92, 0x00, 0x00, 0x00, 0x01, 0x68, 0xee, 0x32, 0xc8, 0xb0};
}

// A keyframe: the parameter sets, then an IDR slice.
inline std::vector<uint8_t> TestH264Keyframe()
{
	auto frame = TestH264Init();
	const uint8_t idr[] = {0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, 0x33, 0xff};
	frame.insert(frame.end(), std::begin(idr), std::end(idr));
	return frame;
}

// An OpusHead (RFC 7845), stereo at 48 kHz.
inline std::vector<uint8_t> TestOpusHead()
{
	return {'O', 'p', 'u', 's', 'H', 'e', 'a', 'd', 1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0};
}
