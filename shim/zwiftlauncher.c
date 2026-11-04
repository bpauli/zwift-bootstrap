/* ZwiftLauncher.exe for Wine: "ZwiftLauncher.exe GAME.exe ARGS..." starts
 * "GAME.exe ARGS..." as a child and waits for it.
 *
 * At start, ZwiftApp.exe reads the image path of its parent process. If the
 * path does not contain "ZwiftLauncher.exe", "ZwiftHelper.exe" or "devenv.exe",
 * the game starts "ZwiftLauncher.exe" and exits, or shows "Could not run Update
 * process" (ERROR ZA1) when that fails. A parent with this name passes the
 * check. See docs/protocol.md.
 *
 * Wine leaves the current directory out of the CreateProcessW search path
 * here, so the game is found with SearchPathW first. */
#include <windows.h>

static WCHAR *skip_arg(WCHAR *p)
{
    if (*p == L'"') {
        for (p++; *p && *p != L'"'; p++) {}
        if (*p) p++;
    } else {
        while (*p && *p != L' ' && *p != L'\t') p++;
    }
    return p;
}

static WCHAR *skip_blanks(WCHAR *p)
{
    while (*p == L' ' || *p == L'\t') p++;
    return p;
}

int main(void)
{
    WCHAR *command = skip_blanks(skip_arg(GetCommandLineW()));
    WCHAR *name = command, *end = skip_arg(command);
    WCHAR buf[MAX_PATH], exe[MAX_PATH];
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;
    DWORD code = 1;
    if (*name == L'"') name++, end--;
    if (end <= name || end - name >= MAX_PATH) return 2;
    lstrcpynW(buf, name, (int)(end - name) + 1);
    ZeroMemory(&si, sizeof si);
    si.cb = sizeof si;
    if (!SearchPathW(NULL, buf, NULL, MAX_PATH, exe, NULL)) return 3;
    if (!CreateProcessW(exe, command, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) return 4;
    WaitForSingleObject(pi.hProcess, INFINITE);
    GetExitCodeProcess(pi.hProcess, &code);
    return (int)code;
}
