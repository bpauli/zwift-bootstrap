/* BleWin10Lib.dll replacement: Bluetooth LE for ZwiftApp.exe under Wine.
 *
 * The original DLL uses WinRT Bluetooth classes that Wine does not implement, so it
 * fails to load. This DLL has the same exports and sends each request as one text line
 * over an AF_UNIX socket to a Linux helper that talks to BlueZ. The helper answers with
 * event lines, and one reader thread turns them into the game's callbacks.
 *
 * The socket path is in ZWIFT_BLE_SOCKET (a Linux path).
 *
 * Protocol: UTF-8 lines, fields separated by tabs, binary data as lowercase hex.
 * Addresses are the 48-bit Bluetooth address as a decimal integer.
 *
 *   to the helper
 *     scan   SERVICE...           SERVICE = uuid=char,char,... (the game's strings)
 *     stop
 *     purge
 *     pair   ADDR
 *     unpair ADDR
 *     write  ADDR CHAR HEX        CHAR is the game's string
 *
 *   from the helper
 *     radio        0|1
 *     adv          ADDR RSSI ADVTYPE SERVICES SECTIONS NAME
 *                  SERVICES = indexes into the scan request, comma separated
 *                  SECTIONS = adtype:hex, comma separated (may be empty)
 *     connected    ADDR MATCH NAME   MATCH = svc:char.char;svc:char (indexes)
 *     notify       ADDR SERVICE CHAR HEX NAME   (strings as the game expects them)
 *     disconnected ADDR UNEXPECTED NAME
 *     log          TEXT
 */
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#if __has_include(<afunix.h>)
#include <afunix.h>
#else
/* MinGW-w64 before version 10 (Ubuntu 22.04) has no afunix.h. */
#define UNIX_PATH_MAX 108
struct sockaddr_un { ADDRESS_FAMILY sun_family; char sun_path[UNIX_PATH_MAX]; };
#endif
#include <windows.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define EXPORT __declspec(dllexport)

/* MSVC STL layouts. The game passes and receives these by pointer. */
typedef struct {
    union { char sso[16]; char *ptr; } buf;
    size_t size;
    size_t capacity;
} msvc_string;

typedef struct { void *begin, *end, *cap_end; } msvc_vector;

typedef struct {
    msvc_string uuid;
    uint8_t *data;
    int32_t len;
} BLECharEntry;

typedef struct {
    msvc_string uuid;
    msvc_vector chars;
} BLEServiceEntry;

typedef struct {
    int32_t type;
    msvc_string address;
    msvc_string name;
    int16_t rssi;
    msvc_vector services;
} BLEMessage;

typedef struct {
    uint8_t type;
    msvc_vector data;
} BLEDataSection;

typedef struct {
    int16_t zero;
    int16_t adv_type;
    msvc_string address;
    msvc_string name;
    msvc_vector sections;
} BLEAdvertisement;

_Static_assert(sizeof(msvc_string) == 0x20, "msvc_string");
_Static_assert(sizeof(BLECharEntry) == 0x30, "BLECharEntry");
_Static_assert(sizeof(BLEServiceEntry) == 0x38, "BLEServiceEntry");
_Static_assert(offsetof(BLEMessage, rssi) == 0x48 && sizeof(BLEMessage) == 0x68, "BLEMessage");
_Static_assert(sizeof(BLEDataSection) == 0x20, "BLEDataSection");
_Static_assert(offsetof(BLEAdvertisement, sections) == 0x48 && sizeof(BLEAdvertisement) == 0x60, "BLEAdvertisement");

enum { MSG_ERROR = 1, MSG_VALUE = 2, MSG_CONNECTED = 3, MSG_DISCONNECTED = 4, MSG_ADVERTISEMENT = 5 };
enum { ERROR_UNEXPECTED_DISCONNECT = 6 };

typedef void (*BLEMessageFn)(const BLEMessage *);
typedef void (*BLEAdvertisementFn)(const BLEAdvertisement *);
typedef void (*BLEErrorFn)(const BLEMessage *, int);
typedef void (*BLEBoolFn)(bool);
typedef void (*BLELogFn)(const char *, va_list);
typedef void (*BLEBondStateFn)(uint64_t, bool);

static BLEMessageFn on_response, on_connection, on_pair;
static BLEAdvertisementFn on_discovery;
static BLEErrorFn on_error;
static BLEBoolFn on_supports, on_available;
static BLELogFn on_log;
static BLEBondStateFn on_bond;

/* The game's scan request, copied. Indexes in helper events refer to it. */
struct req_char { char *uuid; uint8_t *data; int32_t len; };
struct req_service { char *uuid; struct req_char *chars; size_t count; };

#define MAX_DEVICES 64

static CRITICAL_SECTION state_lock;   /* request, scanning, devices */
static struct req_service *request;
static size_t request_count;
static bool scanning;
static uint64_t connected[MAX_DEVICES];

static CRITICAL_SECTION send_lock;
static SOCKET helper = INVALID_SOCKET;
static volatile LONG shut_down;

/* Formats the line here and hands the game a format string without arguments: a MinGW
 * va_list passed on to the game's log function did not read its arguments correctly. */
static void log_msg(const char *fmt, ...)
{
    BLELogFn fn = on_log;
    if (!fn) return;
    char text[512], line[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(text, sizeof text, fmt, ap);
    va_end(ap);
    size_t n = snprintf(line, sizeof line, "[BLEWinLib] ");
    for (const char *p = text; *p && n < sizeof line - 3; p++) {
        if (*p == '%') line[n++] = '%';
        line[n++] = *p;
    }
    line[n] = 0;
    va_start(ap, fmt);
    fn(line, ap);
    va_end(ap);
}

/* --- MSVC objects built for one callback and freed after it ----------------------- */

static void str_set(msvc_string *s, const char *text, size_t n)
{
    memset(s, 0, sizeof *s);
    char *dst = s->buf.sso;
    s->capacity = 15;
    if (n > 15) {
        dst = s->buf.ptr = malloc(n + 1);
        s->capacity = n;
    }
    memcpy(dst, text, n);
    dst[n] = 0;
    s->size = n;
}

static void str_free(msvc_string *s)
{
    if (s->capacity > 15) free(s->buf.ptr);
}

static const char *str_data(const msvc_string *s)
{
    return s->capacity > 15 ? s->buf.ptr : s->buf.sso;
}

static void *vec_alloc(msvc_vector *v, size_t count, size_t size)
{
    char *items = calloc(count ? count : 1, size);
    v->begin = items;
    v->end = v->cap_end = items + count * size;
    return items;
}

static void message_init(BLEMessage *m, int type, const char *address, const char *name)
{
    memset(m, 0, sizeof *m);
    m->type = type;
    str_set(&m->address, address, strlen(address));
    str_set(&m->name, name, strlen(name));
}

static void message_free(BLEMessage *m, bool free_data)
{
    BLEServiceEntry *svc = m->services.begin;
    for (; svc && svc != (BLEServiceEntry *)m->services.end; svc++) {
        BLECharEntry *chr = svc->chars.begin;
        for (; chr != (BLECharEntry *)svc->chars.end; chr++) {
            str_free(&chr->uuid);
            if (free_data) free(chr->data);
        }
        str_free(&svc->uuid);
        free(svc->chars.begin);
    }
    free(m->services.begin);
    str_free(&m->address);
    str_free(&m->name);
}

/* Copy request entry `index` with the characteristics in `chars` (all when NULL). */
static void copy_request_service(BLEServiceEntry *out, size_t index, const size_t *chars, size_t nchars)
{
    const struct req_service *src = &request[index];
    if (!chars) nchars = src->count;
    str_set(&out->uuid, src->uuid, strlen(src->uuid));
    BLECharEntry *dst = vec_alloc(&out->chars, nchars, sizeof *dst);
    for (size_t i = 0; i < nchars; i++) {
        const struct req_char *c = &src->chars[chars ? chars[i] : i];
        str_set(&dst[i].uuid, c->uuid, strlen(c->uuid));
        dst[i].data = c->data;
        dst[i].len = c->len;
    }
}

/* --- Helper connection -------------------------------------------------------------- */

static void send_line(const char *fmt, ...)
{
    char line[4096];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(line, sizeof line - 1, fmt, ap);
    va_end(ap);
    if (n < 0 || n >= (int)sizeof line - 1) {
        log_msg("Dropped a request that is too long");
        return;
    }
    line[n++] = '\n';
    EnterCriticalSection(&send_lock);
    for (int done = 0; helper != INVALID_SOCKET && done < n;) {
        int sent = send(helper, line + done, n - done, 0);
        if (sent <= 0) break;
        done += sent;
    }
    LeaveCriticalSection(&send_lock);
}

static void hex_encode(char *out, const uint8_t *data, size_t n)
{
    static const char digits[] = "0123456789abcdef";
    for (size_t i = 0; i < n; i++) {
        out[2 * i] = digits[data[i] >> 4];
        out[2 * i + 1] = digits[data[i] & 15];
    }
    out[2 * n] = 0;
}

static int hex_nibble(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static uint8_t *hex_decode(const char *hex, int32_t *len)
{
    size_t n = strlen(hex) / 2;
    uint8_t *out = malloc(n ? n : 1);
    for (size_t i = 0; i < n; i++)
        out[i] = (uint8_t)(hex_nibble(hex[2 * i]) << 4 | hex_nibble(hex[2 * i + 1]));
    *len = (int32_t)n;
    return out;
}

/* Build the request line from the copy: scan<TAB>uuid=char,char<TAB>... */
static void send_scan_request(void)
{
    char line[4000];
    size_t used = 0;
    EnterCriticalSection(&state_lock);
    used += snprintf(line + used, sizeof line - used, "scan");
    for (size_t i = 0; i < request_count && used < sizeof line; i++) {
        used += snprintf(line + used, sizeof line - used, "\t%s=", request[i].uuid);
        for (size_t j = 0; j < request[i].count && used < sizeof line; j++)
            used += snprintf(line + used, sizeof line - used, "%s%s", j ? "," : "", request[i].chars[j].uuid);
    }
    LeaveCriticalSection(&state_lock);
    if (used < sizeof line) send_line("%s", line);
    else log_msg("Scan request is too long");
}

static void set_connected(uint64_t addr, bool on)
{
    EnterCriticalSection(&state_lock);
    int free_slot = -1;
    for (int i = 0; i < MAX_DEVICES; i++) {
        if (connected[i] == addr) { if (!on) connected[i] = 0; LeaveCriticalSection(&state_lock); return; }
        if (!connected[i] && free_slot < 0) free_slot = i;
    }
    if (on && free_slot >= 0) connected[free_slot] = addr;
    LeaveCriticalSection(&state_lock);
}

static bool is_connected(uint64_t addr)
{
    bool found = false;
    EnterCriticalSection(&state_lock);
    for (int i = 0; i < MAX_DEVICES && !found; i++) found = connected[i] == addr;
    LeaveCriticalSection(&state_lock);
    return found;
}

/* --- Helper events -> game callbacks (reader thread only) ------------------------- */

static size_t split(char *line, char **fields, size_t max)
{
    size_t n = 0;
    while (n < max) {
        fields[n++] = line;
        char *tab = strchr(line, '\t');
        if (!tab) break;
        *tab = 0;
        line = tab + 1;
    }
    return n;
}

/* Parse "1,4" into indexes below `limit`. */
static size_t parse_indexes(char *list, size_t *out, size_t max, size_t limit)
{
    size_t n = 0;
    for (char *tok = strtok(list, ","); tok && n < max; tok = strtok(NULL, ",")) {
        size_t v = strtoul(tok, NULL, 10);
        if (v < limit) out[n++] = v;
    }
    return n;
}

static void event_advertisement(char **f)
{
    const char *addr = f[1], *name = f[6];
    BLEAdvertisement adv = { .adv_type = (int16_t)atoi(f[3]) };
    str_set(&adv.address, addr, strlen(addr));
    str_set(&adv.name, name, strlen(name));
    size_t nsec = 0;
    for (const char *p = f[5]; *p; p++) nsec += *p == ',';
    nsec += *f[5] != 0;
    BLEDataSection *sec = vec_alloc(&adv.sections, nsec, sizeof *sec);
    size_t used = 0;
    for (char *tok = strtok(f[5], ","); tok && used < nsec; tok = strtok(NULL, ",")) {
        char *colon = strchr(tok, ':');
        if (!colon) continue;
        int32_t len;
        uint8_t *bytes = hex_decode(colon + 1, &len);
        sec[used].type = (uint8_t)strtoul(tok, NULL, 16);
        sec[used].data.begin = bytes;
        sec[used].data.end = sec[used].data.cap_end = bytes + len;
        used++;
    }
    adv.sections.end = adv.sections.cap_end = sec + used;
    if (used && on_discovery) on_discovery(&adv);
    for (size_t i = 0; i < used; i++) free(sec[i].data.begin);
    free(sec);
    str_free(&adv.address);
    str_free(&adv.name);

    BLEMessage m;
    message_init(&m, MSG_ADVERTISEMENT, addr, name);
    m.rssi = (int16_t)atoi(f[2]);
    size_t idx[32];
    EnterCriticalSection(&state_lock);
    size_t n = parse_indexes(f[4], idx, 32, request_count);
    BLEServiceEntry *svc = vec_alloc(&m.services, n, sizeof *svc);
    for (size_t i = 0; i < n; i++) copy_request_service(&svc[i], idx[i], NULL, 0);
    LeaveCriticalSection(&state_lock);
    if (n && on_response) on_response(&m);
    message_free(&m, false);
}

static void event_connected(char **f)
{
    const char *addr = f[1], *name = f[3];
    set_connected(strtoull(addr, NULL, 10), true);

    BLEMessage paired;
    message_init(&paired, MSG_CONNECTED, addr, name);
    if (on_pair) on_pair(&paired);
    message_free(&paired, false);

    BLEMessage m;
    message_init(&m, MSG_CONNECTED, addr, name);
    size_t nsvc = 0;
    for (const char *p = f[2]; *p; p++) nsvc += *p == ';';
    nsvc += *f[2] != 0;
    BLEServiceEntry *svc = vec_alloc(&m.services, nsvc, sizeof *svc);
    size_t used = 0;
    EnterCriticalSection(&state_lock);
    for (char *part = f[2], *next; part && *part && used < nsvc; part = next) {
        next = strchr(part, ';');
        if (next) *next++ = 0;
        char *colon = strchr(part, ':');
        size_t index = strtoul(part, NULL, 10);
        if (!colon || index >= request_count) continue;
        size_t chars[32], count = 0;
        for (char *tok = colon + 1, *dot; tok && *tok && count < 32; tok = dot) {
            dot = strchr(tok, '.');
            if (dot) *dot++ = 0;
            size_t c = strtoul(tok, NULL, 10);
            if (c < request[index].count) chars[count++] = c;
        }
        copy_request_service(&svc[used++], index, chars, count);
    }
    LeaveCriticalSection(&state_lock);
    m.services.end = m.services.cap_end = svc + used;
    if (on_connection) on_connection(&m);
    message_free(&m, false);
}

static void event_value(char **f)
{
    BLEMessage m;
    message_init(&m, MSG_VALUE, f[1], f[5]);
    BLEServiceEntry *svc = vec_alloc(&m.services, 1, sizeof *svc);
    str_set(&svc->uuid, f[2], strlen(f[2]));
    BLECharEntry *chr = vec_alloc(&svc->chars, 1, sizeof *chr);
    str_set(&chr->uuid, f[3], strlen(f[3]));
    chr->data = hex_decode(f[4], &chr->len);
    if (on_response) on_response(&m);
    message_free(&m, true);
}

static void event_disconnected(char **f)
{
    set_connected(strtoull(f[1], NULL, 10), false);
    BLEMessage m;
    message_init(&m, MSG_DISCONNECTED, f[1], f[3]);
    if (atoi(f[2])) {
        if (on_error) on_error(&m, ERROR_UNEXPECTED_DISCONNECT);
    } else if (on_connection) {
        on_connection(&m);
    }
    message_free(&m, false);
}

static bool radio_reported;

static void report_radio(bool on)
{
    if (!radio_reported || !on) {
        if (on_supports) on_supports(on);
        radio_reported = on;
    }
    if (on_available) on_available(on);
    log_msg(on ? "Native BLE - Bluetooth Radio Found." : "Native BLE - Bluetooth Radio NOT Found.");
}

static void handle_line(char *line)
{
    char *f[8];
    size_t n = split(line, f, 8);
    if (shut_down) return;
    if (!strcmp(f[0], "radio") && n == 2) report_radio(atoi(f[1]) != 0);
    else if (!strcmp(f[0], "adv") && n == 7 && *f[6]) event_advertisement(f);
    else if (!strcmp(f[0], "connected") && n == 4 && *f[3]) event_connected(f);
    else if (!strcmp(f[0], "notify") && n == 6 && *f[5]) event_value(f);
    else if (!strcmp(f[0], "disconnected") && n == 4 && *f[3]) event_disconnected(f);
    else if (!strcmp(f[0], "log") && n == 2) log_msg("%s", f[1]);
    else log_msg("Unknown helper event: %s", f[0]);
}

static SOCKET connect_helper(const char *path)
{
    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    /* Wine maps Windows paths in sun_path; Z: is the Linux root. */
    if (snprintf(addr.sun_path, sizeof addr.sun_path, "Z:%s", path) >= (int)sizeof addr.sun_path) return INVALID_SOCKET;
    for (char *p = addr.sun_path; *p; p++) if (*p == '/') *p = '\\';
    SOCKET s = socket(AF_UNIX, SOCK_STREAM, 0);
    if (s != INVALID_SOCKET && connect(s, (struct sockaddr *)&addr, sizeof addr)) {
        closesocket(s);
        s = INVALID_SOCKET;
    }
    return s;
}

static DWORD WINAPI reader(void *unused)
{
    (void)unused;
    WSADATA wsa;
    WSAStartup(MAKEWORD(2, 2), &wsa);
    const char *path = getenv("ZWIFT_BLE_SOCKET");
    if (!path || !*path) {
        log_msg("ZWIFT_BLE_SOCKET is not set, so Bluetooth LE is off");
        report_radio(false);
        return 0;
    }
    bool down_reported = false;
    while (!shut_down) {
        SOCKET s = connect_helper(path);
        if (s == INVALID_SOCKET) {
            if (!down_reported) {
                log_msg("Cannot connect to the Bluetooth helper at %s", path);
                report_radio(false);
                down_reported = true;
            }
            Sleep(2000);
            continue;
        }
        EnterCriticalSection(&send_lock);
        helper = s;
        LeaveCriticalSection(&send_lock);
        down_reported = false;
        log_msg("Connected to the Bluetooth helper");
        EnterCriticalSection(&state_lock);
        bool rescan = scanning;
        LeaveCriticalSection(&state_lock);
        if (rescan) send_scan_request();

        static char buf[65536];
        size_t have = 0;
        for (;;) {
            int got = recv(s, buf + have, (int)(sizeof buf - 1 - have), 0);
            if (got <= 0) break;
            have += got;
            buf[have] = 0;
            char *start = buf, *nl;
            while ((nl = memchr(start, '\n', buf + have - start))) {
                *nl = 0;
                handle_line(start);
                start = nl + 1;
            }
            have -= start - buf;
            memmove(buf, start, have);
            if (have == sizeof buf - 1) have = 0;
        }
        EnterCriticalSection(&send_lock);
        helper = INVALID_SOCKET;
        LeaveCriticalSection(&send_lock);
        closesocket(s);
        if (!shut_down) {
            log_msg("Lost the Bluetooth helper");
            report_radio(false);
        }
    }
    return 0;
}

/* --- Exports ------------------------------------------------------------------------ */

EXPORT void BLEInitBLEFlags(void)
{
    static LONG started;
    if (InterlockedExchange(&started, 1)) return;
    HANDLE t = CreateThread(NULL, 0, reader, NULL, 0, NULL);
    if (t) CloseHandle(t);
}

EXPORT void BLEStartScanning(const msvc_vector *services)
{
    size_t count = services ? (size_t)((BLEServiceEntry *)services->end - (BLEServiceEntry *)services->begin) : 0;
    struct req_service *copy = calloc(count ? count : 1, sizeof *copy);
    for (size_t i = 0; i < count; i++) {
        const BLEServiceEntry *svc = (const BLEServiceEntry *)services->begin + i;
        copy[i].uuid = strdup(str_data(&svc->uuid));
        copy[i].count = (BLECharEntry *)svc->chars.end - (BLECharEntry *)svc->chars.begin;
        copy[i].chars = calloc(copy[i].count ? copy[i].count : 1, sizeof *copy[i].chars);
        for (size_t j = 0; j < copy[i].count; j++) {
            const BLECharEntry *c = (const BLECharEntry *)svc->chars.begin + j;
            copy[i].chars[j] = (struct req_char){ strdup(str_data(&c->uuid)), c->data, c->len };
        }
    }
    EnterCriticalSection(&state_lock);
    struct req_service *old = request;
    size_t old_count = request_count;
    request = copy;
    request_count = count;
    scanning = true;
    LeaveCriticalSection(&state_lock);
    for (size_t i = 0; i < old_count; i++) {
        for (size_t j = 0; j < old[i].count; j++) free(old[i].chars[j].uuid);
        free(old[i].chars);
        free(old[i].uuid);
    }
    free(old);
    send_scan_request();
}

EXPORT void BLEStopScanning(void)
{
    EnterCriticalSection(&state_lock);
    scanning = false;
    LeaveCriticalSection(&state_lock);
    send_line("stop");
}

EXPORT void BLEPurgeDeviceList(void)
{
    send_line("purge");
}

EXPORT bool BLEPairToDevice(uint64_t address)
{
    send_line("pair\t%llu", (unsigned long long)address);
    return true;
}

EXPORT bool BLEUnpairFromDevice(uint64_t address)
{
    bool was_connected = is_connected(address);
    send_line("unpair\t%llu", (unsigned long long)address);
    return was_connected;
}

EXPORT bool BLEWriteToDevice(uint64_t address, const msvc_string *uuid, const msvc_string *data)
{
    if (!is_connected(address)) {
        log_msg("Attempted to write to device %llu but it may have been disconnected.", (unsigned long long)address);
        return false;
    }
    char hex[2 * 512 + 1];
    if (data->size > 512) return false;
    hex_encode(hex, (const uint8_t *)str_data(data), data->size);
    send_line("write\t%llu\t%s\t%s", (unsigned long long)address, str_data(uuid), hex);
    return true;
}

EXPORT void BLERequestBondState(uint64_t address)
{
    if (on_bond) on_bond(address, false);
}

EXPORT void BLEShutdown(void)
{
    InterlockedExchange(&shut_down, 1);
    EnterCriticalSection(&send_lock);
    if (helper != INVALID_SOCKET) shutdown(helper, SD_BOTH);
    LeaveCriticalSection(&send_lock);
}

EXPORT void BLESetAssertFunc(void *fn) { (void)fn; }
EXPORT void BLESetBLEErrorFunc(BLEErrorFn fn) { on_error = fn; }
EXPORT void BLESetBondStateFunc(BLEBondStateFn fn) { on_bond = fn; }
EXPORT void BLESetConnectionStatusCBFunc(BLEMessageFn fn) { on_connection = fn; }
EXPORT void BLESetEnableBLEIsAvailableFunc(BLEBoolFn fn) { on_available = fn; }
EXPORT void BLESetEnableSupportsBLEFunc(BLEBoolFn fn) { on_supports = fn; }
EXPORT void BLESetLogFunc(BLELogFn fn) { on_log = fn; }
EXPORT void BLESetOnPairCB(BLEMessageFn fn) { on_pair = fn; }
EXPORT void BLESetPeripheralDiscoveryFunc(BLEAdvertisementFn fn) { on_discovery = fn; }
EXPORT void BLESetProcessBLEResponse(BLEMessageFn fn) { on_response = fn; }

BOOL WINAPI DllMain(HINSTANCE dll, DWORD reason, void *reserved)
{
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) {
        DisableThreadLibraryCalls(dll);
        InitializeCriticalSection(&state_lock);
        InitializeCriticalSection(&send_lock);
    }
    return TRUE;
}
