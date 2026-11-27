#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static VOID WINAPI win7_file_time(LPFILETIME time) {
    GetSystemTimeAsFileTime(time);
}

VOID WINAPI __wrap_GetSystemTimePreciseAsFileTime(LPFILETIME time) {
    win7_file_time(time);
}

VOID (WINAPI *__wrap___imp_GetSystemTimePreciseAsFileTime)(LPFILETIME) = win7_file_time;
