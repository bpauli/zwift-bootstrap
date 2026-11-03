/* Loads libusb0.dll the way ANT_DLL.dll does, opens and closes the stick,
 * unloads the DLL, then calls CreateFileW through this program's import.
 * The shim hooks that import, so the call must still work after unload.
 * Needs a plugged-in ANT stick. Run from a dir that holds libusb0.dll:
 *   x86_64-w64-mingw32-gcc -O2 -o unloadtest.exe unloadtest.c
 *   wine unloadtest.exe
 * Pass: "CreateFileW after unload: ok". */
#include <windows.h>
#include <stdio.h>

struct usb_device;
struct usb_bus { struct usb_bus *next, *prev; char dirname[512]; struct usb_device *devices; };

int main(int argc, char **argv)
{
    const char *dll = argc > 1 ? argv[1] : "libusb0.dll";
    HMODULE lib = LoadLibraryA(dll);
    if (!lib) { printf("load failed %lu\n", GetLastError()); return 1; }

    void (*init)(void) = (void *)GetProcAddress(lib, "usb_init");
    int (*find_busses)(void) = (void *)GetProcAddress(lib, "usb_find_busses");
    int (*find_devices)(void) = (void *)GetProcAddress(lib, "usb_find_devices");
    struct usb_bus *(*get_busses)(void) = (void *)GetProcAddress(lib, "usb_get_busses");
    void *(*open)(struct usb_device *) = (void *)GetProcAddress(lib, "usb_open");
    int (*close)(void *) = (void *)GetProcAddress(lib, "usb_close");

    init();
    find_busses();
    find_devices();
    struct usb_bus *bus = get_busses();
    if (!bus || !bus->devices) { printf("no stick\n"); return 1; }
    void *h = open(bus->devices);
    if (!h) { printf("open failed\n"); return 1; }
    close(h);

    FreeLibrary(lib);
    printf("still loaded after FreeLibrary: %s\n", GetModuleHandleA(dll) ? "yes" : "no");
    fflush(stdout);

    HANDLE f = CreateFileW(L"NUL", GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("CreateFileW after unload: %s\n", f != INVALID_HANDLE_VALUE ? "ok" : "failed");
    if (f != INVALID_HANDLE_VALUE) CloseHandle(f);
    return f != INVALID_HANDLE_VALUE ? 0 : 1;
}
