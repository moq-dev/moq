/*
 * Copyright © Microsoft Corporation
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice (including the next
 * paragraph) shall be included in all copies or substantial portions of the
 * Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 */

#ifndef _COMPAT_WIN32_H_
#define _COMPAT_WIN32_H_

#include <windows.h>
#include <winsock.h>
#include <io.h>
#include <time.h>

// Include stdlib.h here to make sure we
// always replace MSVC getenv definition
// with the macro / _getenv inline below
#include <stdlib.h>

#ifdef __cplusplus
extern "C" {
#endif

#define F_OK 0
#define RTLD_NOW 0
#define RTLD_GLOBAL 0

#ifndef NTSTATUS
typedef LONG NTSTATUS;
#define STATUS_SUCCESS  ((NTSTATUS)0x00000000L)
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023)
#define NT_SUCCESS(status) (((NTSTATUS)(status)) >= 0)
#endif

typedef unsigned int __uid_t;

#if _MSC_VER
#define getenv _getenv
#define secure_getenv _getenv
#define HAVE_SECURE_GETENV
inline char* _getenv(const char *varname)
{
    static char _getenv_buf[32767];
    return GetEnvironmentVariableA(varname, &_getenv_buf[0], sizeof(_getenv_buf)) ? &_getenv_buf[0] : NULL;
}
#endif

#ifdef _MSC_VER
inline char* strtok_r(char *s, const char *delim, char **save_ptr)
{
    return strtok_s(s, delim, save_ptr);
}
#endif

inline void* dlopen(const char *file, int mode)
{
    return LoadLibrary(file);
}

inline int dlclose(void *handle)
{
    return FreeLibrary(handle);
}

inline void *dlsym(void *handle, const char *name)
{
    return GetProcAddress(handle, name);
}

inline static char* dlerror()
{
    static char last_err_string[512];
    memset(last_err_string, '\0', 512);
    FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM, NULL, GetLastError(),
                   MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT), last_err_string, (512 - 1), NULL);
    return last_err_string;
}

inline __uid_t geteuid()
{
    return 0;
};

inline __uid_t getuid()
{
    return 0;
};

typedef DWORD pid_t;
typedef CRITICAL_SECTION pthread_mutex_t;

inline void pthread_mutex_lock(pthread_mutex_t* a)
{
    EnterCriticalSection(a);
}

inline void pthread_mutex_unlock(pthread_mutex_t* a)
{
    LeaveCriticalSection(a);
}

inline void pthread_mutex_init(pthread_mutex_t* a, void* attr)
{
    InitializeCriticalSection(a);
}

inline void pthread_mutex_destroy(pthread_mutex_t* a)
{
    DeleteCriticalSection(a);
}

inline int gettimeofday(struct timeval *tv, struct timezone* tz)
{
    SYSTEMTIME st;
    GetSystemTime(&st);
    tv->tv_sec = st.wSecond;
    tv->tv_usec = st.wMilliseconds * 1000; /* milli to micro sec */
    return 0;
}

#ifdef _MSC_VER
#include <d3dkmthk.h>
#include <d3dukmdt.h>
#else
//
// DDI level handle that represents a kernel mode object (allocation, device, etc)
//
typedef UINT D3DKMT_HANDLE;

typedef struct _D3DKMT_CLOSEADAPTER {
    D3DKMT_HANDLE   hAdapter;   // in: adapter handle
} D3DKMT_CLOSEADAPTER;

typedef struct _D3DKMT_OPENADAPTERFROMLUID {
    LUID            AdapterLuid;
    D3DKMT_HANDLE   hAdapter;
} D3DKMT_OPENADAPTERFROMLUID;

typedef enum _KMTQUERYADAPTERINFOTYPE {
    KMTQAITYPE_QUERYREGISTRY           = 48,
} KMTQUERYADAPTERINFOTYPE;

typedef struct _D3DKMT_QUERYADAPTERINFO {
    D3DKMT_HANDLE           hAdapter;
    KMTQUERYADAPTERINFOTYPE Type;
    VOID*                   pPrivateDriverData;
    UINT                    PrivateDriverDataSize;
} D3DKMT_QUERYADAPTERINFO;

typedef struct _D3DDDI_QUERYREGISTRY_FLAGS {
    union {
        struct {
            UINT   TranslatePath    :  1;
            UINT   MutableValue     :  1;
            UINT   Reserved         : 30;
        };
        UINT Value;
    };
} D3DDDI_QUERYREGISTRY_FLAGS;

typedef enum _D3DDDI_QUERYREGISTRY_TYPE {
    D3DDDI_QUERYREGISTRY_ADAPTERKEY      = 1,
} D3DDDI_QUERYREGISTRY_TYPE;

typedef enum _D3DDDI_QUERYREGISTRY_STATUS {
    D3DDDI_QUERYREGISTRY_STATUS_SUCCESS              = 0,
    D3DDDI_QUERYREGISTRY_STATUS_BUFFER_OVERFLOW      = 1,
} D3DDDI_QUERYREGISTRY_STATUS;

typedef struct _D3DDDI_QUERYREGISTRY_INFO {
    D3DDDI_QUERYREGISTRY_TYPE    QueryType;              // In
    D3DDDI_QUERYREGISTRY_FLAGS   QueryFlags;             // In
    WCHAR                        ValueName[MAX_PATH];    // In
    ULONG                        ValueType;              // In
    ULONG                        PhysicalAdapterIndex;   // In
    ULONG                        OutputValueSize;        // Out. Number of bytes written to the output value or required in case of D3DDDI_QUERYREGISTRY_STATUS_BUFFER_OVERFLOW.
    D3DDDI_QUERYREGISTRY_STATUS  Status;                 // Out
    union {
        DWORD   OutputDword;                            // Out
        UINT64  OutputQword;                            // Out
        WCHAR   OutputString[1];                        // Out. Dynamic array
        BYTE    OutputBinary[1];                        // Out. Dynamic array
    };
} D3DDDI_QUERYREGISTRY_INFO;

typedef _Check_return_ NTSTATUS(APIENTRY *PFND3DKMT_CLOSEADAPTER)(_In_ CONST D3DKMT_CLOSEADAPTER*);
typedef _Check_return_ NTSTATUS(APIENTRY *PFND3DKMT_OPENADAPTERFROMLUID)(_Inout_ D3DKMT_OPENADAPTERFROMLUID*);
typedef _Check_return_ NTSTATUS(APIENTRY *PFND3DKMT_QUERYADAPTERINFO)(_Inout_ CONST D3DKMT_QUERYADAPTERINFO*);

#endif

#ifdef __cplusplus
}
#endif

#endif /* _COMPAT_WIN32_H_ */
