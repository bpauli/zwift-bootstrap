/* libusb0.dll replacement for Wine: gives the libusb-win32 0.1 calls that
 * ANT_DLL.dll uses and sends the data through the Linux serial device of the
 * ANT USB stick (kernel driver usb_serial_simple).
 *
 * Wine also shows that tty as a COM port, and Zwift's serial port probe for
 * Zwift Protocol devices opens every COM port. A second reader takes the ANT
 * replies, so while the DLL has the stick open it refuses other opens of the
 * same device from ZwiftApp.exe (a hook on its CreateFileA/W imports).
 *
 * ANT_PORT sets the device: a Linux path (/dev/ttyUSB0) or a COM port (COM5).
 * The default is the first /dev/serial/by-id/usb-Dynastream_Innovations_* link. */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define LIBUSB_PATH_MAX 512
#define USB_ETIMEDOUT 116
#define USB_EIO 5

struct usb_device_descriptor {
    unsigned char bLength, bDescriptorType;
    unsigned short bcdUSB;
    unsigned char bDeviceClass, bDeviceSubClass, bDeviceProtocol, bMaxPacketSize0;
    unsigned short idVendor, idProduct, bcdDevice;
    unsigned char iManufacturer, iProduct, iSerialNumber, bNumConfigurations;
};

struct usb_bus;

struct usb_device {
    struct usb_device *next, *prev;
    char filename[LIBUSB_PATH_MAX];
    struct usb_bus *bus;
    struct usb_device_descriptor descriptor;
    void *config;
    void *dev;
    unsigned char devnum, num_children;
    struct usb_device **children;
};

struct usb_bus {
    struct usb_bus *next, *prev;
    char dirname[LIBUSB_PATH_MAX];
    struct usb_device *devices;
    unsigned long location;
    struct usb_device *root_dev;
};

typedef struct usb_dev_handle {
    HANDLE port;
    struct usb_device *device;
    BOOL counted;
} usb_dev_handle;

typedef struct {
    usb_dev_handle *handle;
    unsigned char *bytes;
    int size;
} async_context;

static struct usb_bus bus;
static struct usb_device ant_stick;
static struct usb_bus *busses;

static int port_path(char *path, size_t size)
{
    char name[MAX_PATH];
    DWORD n = GetEnvironmentVariableA("ANT_PORT", name, sizeof name);

    if (n > 0 && n < sizeof name) {
        if (name[0] == '/') {
            for (char *c = name; *c; c++) if (*c == '/') *c = '\\';
            return snprintf(path, size, "\\\\?\\unix%s", name) < (int)size;
        }
        if (name[0] == '\\') return snprintf(path, size, "%s", name) < (int)size;
        return snprintf(path, size, "\\\\.\\%s", name) < (int)size;
    }

    WIN32_FIND_DATAA found;
    HANDLE find = FindFirstFileA("Z:\\dev\\serial\\by-id\\usb-Dynastream_Innovations_*", &found);
    if (find == INVALID_HANDLE_VALUE) return 0;
    FindClose(find);
    return snprintf(path, size, "\\\\?\\unix\\dev\\serial\\by-id\\%s", found.cFileName) < (int)size;
}

/* Identity of the open stick: Wine gives the inode of the tty as file index. */
static volatile LONG stick_handles;
static BY_HANDLE_FILE_INFORMATION stick_id;

static HANDLE (WINAPI *real_CreateFileA)(LPCSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD, DWORD, HANDLE);
static HANDLE (WINAPI *real_CreateFileW)(LPCWSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD, DWORD, HANDLE);

static HANDLE refuse_if_stick(HANDLE h)
{
    BY_HANDLE_FILE_INFORMATION id;

    if (h == INVALID_HANDLE_VALUE || !stick_handles) return h;
    if (!GetFileInformationByHandle(h, &id)) return h;
    if (id.dwVolumeSerialNumber != stick_id.dwVolumeSerialNumber ||
        id.nFileIndexHigh != stick_id.nFileIndexHigh ||
        id.nFileIndexLow != stick_id.nFileIndexLow) return h;

    CloseHandle(h);
    SetLastError(ERROR_ACCESS_DENIED);
    return INVALID_HANDLE_VALUE;
}

static HANDLE WINAPI hook_CreateFileA(LPCSTR name, DWORD access, DWORD share, LPSECURITY_ATTRIBUTES sa,
                                      DWORD disposition, DWORD flags, HANDLE template_file)
{
    HANDLE h = real_CreateFileA(name, access, share, sa, disposition, flags, template_file);
    return name && !strncmp(name, "\\\\.\\", 4) ? refuse_if_stick(h) : h;
}

static HANDLE WINAPI hook_CreateFileW(LPCWSTR name, DWORD access, DWORD share, LPSECURITY_ATTRIBUTES sa,
                                      DWORD disposition, DWORD flags, HANDLE template_file)
{
    HANDLE h = real_CreateFileW(name, access, share, sa, disposition, flags, template_file);
    return name && !wcsncmp(name, L"\\\\.\\", 4) ? refuse_if_stick(h) : h;
}

static void patch_import(void **slot, void *hook, void **real)
{
    DWORD old;
    if (*slot == hook) return;
    *real = *slot;
    if (!VirtualProtect(slot, sizeof *slot, PAGE_READWRITE, &old)) return;
    *slot = hook;
    VirtualProtect(slot, sizeof *slot, old, &old);
}

/* Point the KERNEL32 CreateFileA/W imports of the main executable at the hooks. */
static void hook_main_module_imports(void)
{
    static LONG done;
    if (InterlockedExchange(&done, 1)) return;

    /* The hooks stay in the executable's imports after ANT_DLL.dll frees this
     * DLL at exit, so it must never unload. */
    HMODULE self;
    if (!GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                            (LPCSTR)hook_CreateFileA, &self)) return;

    BYTE *base = (BYTE *)GetModuleHandleA(NULL);
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(base + ((IMAGE_DOS_HEADER *)base)->e_lfanew);
    IMAGE_DATA_DIRECTORY dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    if (!dir.VirtualAddress) return;

    for (IMAGE_IMPORT_DESCRIPTOR *imp = (IMAGE_IMPORT_DESCRIPTOR *)(base + dir.VirtualAddress); imp->Name; imp++) {
        if (_stricmp((char *)(base + imp->Name), "KERNEL32.dll") || !imp->OriginalFirstThunk) continue;
        IMAGE_THUNK_DATA *names = (IMAGE_THUNK_DATA *)(base + imp->OriginalFirstThunk);
        IMAGE_THUNK_DATA *slots = (IMAGE_THUNK_DATA *)(base + imp->FirstThunk);
        for (; names->u1.AddressOfData; names++, slots++) {
            if (IMAGE_SNAP_BY_ORDINAL(names->u1.Ordinal)) continue;
            const char *fn = (char *)((IMAGE_IMPORT_BY_NAME *)(base + names->u1.AddressOfData))->Name;
            if (!strcmp(fn, "CreateFileA"))
                patch_import((void **)&slots->u1.Function, (void *)hook_CreateFileA, (void **)&real_CreateFileA);
            else if (!strcmp(fn, "CreateFileW"))
                patch_import((void **)&slots->u1.Function, (void *)hook_CreateFileW, (void **)&real_CreateFileW);
        }
    }
}

static int port_exists(void)
{
    char path[MAX_PATH + 32];
    return port_path(path, sizeof path);
}

void usb_init(void)
{
    strcpy(bus.dirname, "bus-0");
    bus.devices = &ant_stick;
    bus.root_dev = &ant_stick;

    strcpy(ant_stick.filename, "ant-usb-serial");
    ant_stick.bus = &bus;
    ant_stick.descriptor = (struct usb_device_descriptor){
        .bLength = 18, .bDescriptorType = 1, .bcdUSB = 0x0200,
        .bMaxPacketSize0 = 32, .idVendor = 0x0fcf, .idProduct = 0x1008,
        .bcdDevice = 0x0100, .iManufacturer = 1, .iProduct = 2,
        .iSerialNumber = 3, .bNumConfigurations = 1,
    };
    ant_stick.devnum = 1;
}

void usb_set_debug(int level) { (void)level; }

int usb_find_busses(void)
{
    int changed = busses == NULL;
    busses = &bus;
    return changed;
}

int usb_find_devices(void)
{
    struct usb_device *before = bus.devices;
    bus.devices = port_exists() ? &ant_stick : NULL;
    return before != bus.devices;
}

struct usb_bus *usb_get_busses(void) { return busses; }

struct usb_device *usb_device(usb_dev_handle *h) { return h->device; }

usb_dev_handle *usb_open(struct usb_device *dev)
{
    char path[MAX_PATH + 32];
    if (!port_path(path, sizeof path)) return NULL;

    HANDLE port = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, 0, NULL,
                              OPEN_EXISTING, FILE_FLAG_OVERLAPPED, NULL);
    if (port == INVALID_HANDLE_VALUE) return NULL;
    BOOL counted = GetFileInformationByHandle(port, &stick_id);
    if (counted) {
        InterlockedIncrement(&stick_handles);
        hook_main_module_imports();
    }

    DCB dcb = { .DCBlength = sizeof dcb };
    GetCommState(port, &dcb);
    dcb.BaudRate = 57600;
    dcb.ByteSize = 8;
    dcb.Parity = NOPARITY;
    dcb.StopBits = ONESTOPBIT;
    dcb.fBinary = TRUE;
    dcb.fOutxCtsFlow = FALSE;
    dcb.fRtsControl = RTS_CONTROL_ENABLE;
    dcb.fDtrControl = DTR_CONTROL_ENABLE;
    dcb.fOutX = dcb.fInX = FALSE;
    SetCommState(port, &dcb);
    PurgeComm(port, PURGE_RXCLEAR | PURGE_TXCLEAR);
    /* A read returns the bytes that are there, once there is one. */
    COMMTIMEOUTS t = { MAXDWORD, 0, 0, 0, 0 };
    SetCommTimeouts(port, &t);

    usb_dev_handle *h = calloc(1, sizeof *h);
    h->port = port;
    h->device = dev;
    h->counted = counted;
    return h;
}

int usb_close(usb_dev_handle *h)
{
    if (!h) return -USB_EIO;
    if (h->counted) InterlockedDecrement(&stick_handles);
    CloseHandle(h->port);
    free(h);
    return 0;
}

int usb_set_configuration(usb_dev_handle *h, int configuration) { (void)h; (void)configuration; return 0; }
int usb_claim_interface(usb_dev_handle *h, int iface) { (void)h; (void)iface; return 0; }
int usb_release_interface(usb_dev_handle *h, int iface) { (void)h; (void)iface; return 0; }
int usb_clear_halt(usb_dev_handle *h, unsigned int ep) { (void)h; (void)ep; return 0; }

int usb_reset(usb_dev_handle *h)
{
    PurgeComm(h->port, PURGE_RXCLEAR | PURGE_TXCLEAR);
    return 0;
}

int usb_get_string_simple(usb_dev_handle *h, int index, char *buf, size_t buflen)
{
    (void)h;
    static const char *strings[] = { NULL, "Dynastream Innovations", "ANT USBStick2", "123" };
    if (index < 1 || index > 3 || buflen == 0) return -USB_EIO;
    lstrcpynA(buf, strings[index], (int)buflen);
    return (int)strlen(buf);
}

/* Wine ignores COMMTIMEOUTS for overlapped serial reads: a read waits until
 * at least one byte is there. So the read timeout is a wait on the read,
 * then a cancel. */
static int port_io(usb_dev_handle *h, int write, void *bytes, int size, int timeout)
{
    OVERLAPPED ov = { .hEvent = CreateEventA(NULL, TRUE, FALSE, NULL) };
    DWORD n = 0;
    DWORD wait = timeout > 0 ? (DWORD)timeout : 1;
    BOOL ok = write ? WriteFile(h->port, bytes, (DWORD)size, NULL, &ov)
                    : ReadFile(h->port, bytes, (DWORD)size, NULL, &ov);
    BOOL pending = !ok && GetLastError() == ERROR_IO_PENDING;

    if (pending) {
        if (!write && WaitForSingleObject(ov.hEvent, wait) == WAIT_TIMEOUT)
            CancelIoEx(h->port, &ov);
        ok = GetOverlappedResult(h->port, &ov, &n, TRUE);
    } else if (ok) {
        GetOverlappedResult(h->port, &ov, &n, FALSE);
    }
    CloseHandle(ov.hEvent);

    if (write) return ok ? (int)n : -USB_EIO;
    if (n > 0) return (int)n;
    /* A failed or cancelled read is a timeout for ANT_DLL: an error stops its
     * receive thread for good. A read that failed at once (the stick is gone)
     * still takes the timeout, so that the thread does not spin. */
    if (!pending) Sleep(wait);
    SetLastError(ERROR_TIMEOUT);
    return -USB_ETIMEDOUT;
}

int usb_interrupt_write(usb_dev_handle *h, int ep, char *bytes, int size, int timeout)
{
    (void)ep; (void)timeout;
    return port_io(h, 1, bytes, size, 0);
}

int usb_bulk_write(usb_dev_handle *h, int ep, char *bytes, int size, int timeout)
{
    (void)ep; (void)timeout;
    return port_io(h, 1, bytes, size, 0);
}

int usb_bulk_read(usb_dev_handle *h, int ep, char *bytes, int size, int timeout)
{
    (void)ep;
    return port_io(h, 0, bytes, size, timeout);
}

int usb_interrupt_read(usb_dev_handle *h, int ep, char *bytes, int size, int timeout)
{
    (void)ep;
    return port_io(h, 0, bytes, size, timeout);
}

int usb_bulk_setup_async(usb_dev_handle *h, void **context, unsigned char ep)
{
    (void)ep;
    async_context *c = calloc(1, sizeof *c);
    c->handle = h;
    *context = c;
    return 0;
}

int usb_submit_async(void *context, char *bytes, int size)
{
    async_context *c = context;
    c->bytes = (unsigned char *)bytes;
    c->size = size;
    return 0;
}

int usb_reap_async_nocancel(void *context, int timeout)
{
    async_context *c = context;
    return port_io(c->handle, 0, c->bytes, c->size, timeout);
}

int usb_reap_async(void *context, int timeout) { return usb_reap_async_nocancel(context, timeout); }
int usb_cancel_async(void *context) { (void)context; return 0; }

int usb_free_async(void **context)
{
    free(*context);
    *context = NULL;
    return 0;
}

const char *usb_strerror(void) { return "libusb0 serial shim error"; }
