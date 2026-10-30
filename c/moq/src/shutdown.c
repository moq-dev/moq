#include <moq/moq.h>

/* Exported by the moq-ffi staticlib, but not a uniffi export, so the generated header does not
 * declare it. */
void moq_ffi_shutdown(void);

void moq_shutdown(void)
{
	moq_ffi_shutdown();
	moq_shutdown_dispatcher();
}
