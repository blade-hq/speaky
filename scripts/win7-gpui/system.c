#define WIN32_LEAN_AND_MEAN
#include <windows.h>

struct DISK_SPACE_INFORMATION;

HANDLE WINAPI __wrap_CreateFileMappingFromApp(
    HANDLE file, PSECURITY_ATTRIBUTES security, ULONG protection,
    ULONG64 max_size, PCWSTR name
) {
    /* Win7 does not support image mappings that suppress driver notifications. */
    const ULONG image_no_execute = SEC_IMAGE | SEC_NOCACHE;
    if ((protection & image_no_execute) == image_no_execute) {
        SetLastError(ERROR_NOT_SUPPORTED);
        return NULL;
    }
    return CreateFileMappingW(file, security, protection, (DWORD)(max_size >> 32),
                              (DWORD)max_size, name);
}

PVOID WINAPI __wrap_MapViewOfFileFromApp(
    HANDLE mapping, ULONG access, ULONG64 offset, SIZE_T size
) {
    return MapViewOfFile(mapping, access, (DWORD)(offset >> 32), (DWORD)offset, size);
}

HRESULT WINAPI __wrap_GetDiskSpaceInformationW(
    LPCWSTR root, struct DISK_SPACE_INFORMATION *space
) {
    (void)root;
    (void)space;
    SetLastError(ERROR_CALL_NOT_IMPLEMENTED);
    return HRESULT_FROM_WIN32(ERROR_CALL_NOT_IMPLEMENTED);
}

BOOL WINAPI __wrap_GetFirmwareType(PFIRMWARE_TYPE type) {
    (void)type;
    SetLastError(ERROR_CALL_NOT_IMPLEMENTED);
    return FALSE;
}

BOOL WINAPI __wrap_IsNativeVhdBoot(PBOOL native_vhd_boot) {
    (void)native_vhd_boot;
    SetLastError(ERROR_CALL_NOT_IMPLEMENTED);
    return FALSE;
}

__typeof__(&__wrap_CreateFileMappingFromApp) __wrap___imp_CreateFileMappingFromApp = __wrap_CreateFileMappingFromApp;
__typeof__(&__wrap_MapViewOfFileFromApp) __wrap___imp_MapViewOfFileFromApp = __wrap_MapViewOfFileFromApp;
__typeof__(&__wrap_GetDiskSpaceInformationW) __wrap___imp_GetDiskSpaceInformationW = __wrap_GetDiskSpaceInformationW;
__typeof__(&__wrap_GetFirmwareType) __wrap___imp_GetFirmwareType = __wrap_GetFirmwareType;
__typeof__(&__wrap_IsNativeVhdBoot) __wrap___imp_IsNativeVhdBoot = __wrap_IsNativeVhdBoot;
