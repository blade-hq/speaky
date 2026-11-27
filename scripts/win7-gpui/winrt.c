#define WIN32_LEAN_AND_MEAN
#include <windows.h>

typedef HRESULT (WINAPI *GetActivationFactory)(void *, const IID *, void **);
typedef BOOL (WINAPI *OriginateError)(HRESULT, UINT, const WCHAR *);
typedef HRESULT (WINAPI *IncrementMTAUsage)(void **);

static FARPROC resolve_winrt(const char *name) {
    HMODULE module = GetModuleHandleW(L"combase.dll");
    if (module == NULL) {
        WCHAR path[MAX_PATH];
        static const WCHAR suffix[] = L"\\combase.dll";
        UINT length = GetSystemDirectoryW(path, ARRAYSIZE(path));
        if (!length || length >= ARRAYSIZE(path) - ARRAYSIZE(suffix)) {
            return NULL;
        }
        for (UINT i = 0; i < ARRAYSIZE(suffix); ++i) {
            path[length + i] = suffix[i];
        }
        module = LoadLibraryW(path);
    }
    return module ? GetProcAddress(module, name) : NULL;
}

HRESULT WINAPI __wrap_RoGetActivationFactory(void *class_id, const IID *iid, void **factory) {
    GetActivationFactory real = (GetActivationFactory)resolve_winrt("RoGetActivationFactory");
    if (real != NULL) {
        return real(class_id, iid, factory);
    }
    if (factory != NULL) {
        *factory = NULL;
    }
    return E_NOTIMPL;
}

BOOL WINAPI __wrap_RoOriginateErrorW(HRESULT code, UINT length, const WCHAR *message) {
    OriginateError real = (OriginateError)resolve_winrt("RoOriginateErrorW");
    return real != NULL && real(code, length, message);
}

HRESULT WINAPI __wrap_CoIncrementMTAUsage(void **cookie) {
    HMODULE ole = GetModuleHandleW(L"ole32.dll");
    IncrementMTAUsage real = ole ? (IncrementMTAUsage)GetProcAddress(ole, "CoIncrementMTAUsage") : NULL;
    if (real != NULL) {
        return real(cookie);
    }
    if (cookie != NULL) {
        *cookie = NULL;
    }
    return E_NOTIMPL;
}

GetActivationFactory __wrap___imp_RoGetActivationFactory = __wrap_RoGetActivationFactory;
OriginateError __wrap___imp_RoOriginateErrorW = __wrap_RoOriginateErrorW;
IncrementMTAUsage __wrap___imp_CoIncrementMTAUsage = __wrap_CoIncrementMTAUsage;
