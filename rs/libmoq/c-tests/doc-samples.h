// The inputs the C documentation samples leave to the reader. `just rs c-tests`
// compiles every C sample in doc/lib/c beside this header, so a sample that
// names a missing symbol or passes the wrong arguments fails. Never executed.
#include <moq.h>

#include <stddef.h>
#include <stdint.h>
#include <string.h>

static const char *hex_sha256;
static const char *url;
static size_t url_len;
static uint32_t origin;
static void *user_data;

static void on_status(void *user_data, int32_t code) {
	(void)user_data;
	(void)code;
}

static int fail(const char *reason) {
	(void)reason;
	return -1;
}
