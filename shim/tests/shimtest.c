/* Drives libusb0.dll like ANT_DLL.dll: a receive thread that reaps async
 * reads with a 1 s timeout, and a main thread that sends ANT commands.
 * Needs a plugged-in ANT stick. Run from a dir that holds libusb0.dll:
 *   x86_64-w64-mingw32-gcc -O2 -o shimtest.exe shimtest.c -L. -lusb0
 *   wine shimtest.exe 5
 * Pass: every command gets a reply within milliseconds, idle reads end at
 * 1000 ms, and the last line says receive_thread=ended. */
#include <windows.h>
#include <stdio.h>

struct usb_device;
struct usb_bus { struct usb_bus *next, *prev; char dirname[512]; struct usb_device *devices; };

void usb_init(void);
int usb_find_busses(void);
int usb_find_devices(void);
struct usb_bus *usb_get_busses(void);
void *usb_open(struct usb_device *dev);
int usb_close(void *h);
int usb_bulk_write(void *h, int ep, char *bytes, int size, int timeout);
int usb_bulk_setup_async(void *h, void **context, unsigned char ep);
int usb_submit_async(void *context, char *bytes, int size);
int usb_reap_async_nocancel(void *context, int timeout);

static LARGE_INTEGER freq, start;
static volatile LONG stop, reads, timeouts, errors;
static volatile LONG longest_ms;

static double now_ms(void)
{
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return (t.QuadPart - start.QuadPart) * 1000.0 / freq.QuadPart;
}

static DWORD WINAPI receive(void *h)
{
    void *ctx;
    unsigned char buf[64];
    usb_bulk_setup_async(h, &ctx, 0x81);
    while (!stop) {
        usb_submit_async(ctx, (char *)buf, sizeof buf);
        double t0 = now_ms();
        int n = usb_reap_async_nocancel(ctx, 1000);
        LONG took = (LONG)(now_ms() - t0);
        if (took > longest_ms) longest_ms = took;
        if (n > 0) {
            InterlockedIncrement(&reads);
            printf("%9.1f rx %2d:", now_ms(), n);
            for (int i = 0; i < n; i++) printf(" %02X", buf[i]);
            printf("  (waited %ld ms)\n", took);
        } else if (n == -116) {
            InterlockedIncrement(&timeouts);
        } else {
            InterlockedIncrement(&errors);
            printf("%9.1f rx error %d\n", now_ms(), n);
        }
        fflush(stdout);
    }
    return 0;
}

static void send_msg(void *h, const char *what, const unsigned char *msg, int len)
{
    printf("%9.1f tx %s\n", now_ms(), what);
    fflush(stdout);
    if (usb_bulk_write(h, 0x01, (char *)msg, len, 1000) != len) printf("  write failed\n");
}

int main(int argc, char **argv)
{
    int idle_s = argc > 1 ? atoi(argv[1]) : 5;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&start);

    usb_init();
    usb_find_busses();
    usb_find_devices();
    struct usb_bus *bus = usb_get_busses();
    if (!bus || !bus->devices) { printf("no stick\n"); return 1; }
    void *h = usb_open(bus->devices);
    if (!h) { printf("open failed %lu\n", GetLastError()); return 1; }

    HANDLE thread = CreateThread(NULL, 0, receive, h, 0, NULL);
    static const unsigned char reset[] = { 0xA4, 0x01, 0x4A, 0x00, 0xEF };
    static const unsigned char caps[] = { 0xA4, 0x02, 0x4D, 0x00, 0x54, 0xBF };
    static const unsigned char assign[] = { 0xA4, 0x03, 0x42, 0x00, 0x00, 0x00, 0xE5 };
    static const unsigned char unassign[] = { 0xA4, 0x01, 0x41, 0x00, 0xE4 };

    send_msg(h, "reset", reset, sizeof reset);
    Sleep(1000);
    send_msg(h, "request capabilities", caps, sizeof caps);
    Sleep(500);
    printf("%9.1f idle %d s (reads must keep timing out, not hang)\n", now_ms(), idle_s);
    fflush(stdout);
    Sleep(idle_s * 1000);
    send_msg(h, "assign channel 0", assign, sizeof assign);
    Sleep(500);
    send_msg(h, "unassign channel 0", unassign, sizeof unassign);
    Sleep(1500);

    stop = 1;
    DWORD r = WaitForSingleObject(thread, 3000);
    printf("reads=%ld timeouts=%ld errors=%ld longest_wait_ms=%ld receive_thread=%s\n",
           reads, timeouts, errors, longest_ms, r == WAIT_OBJECT_0 ? "ended" : "STUCK");
    usb_close(h);
    return r == WAIT_OBJECT_0 ? 0 : 2;
}
