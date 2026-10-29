// bridge.c — the `extern "C"` layer between a Jeti sketch and the board API.
//
// On real hardware this file (or its equivalent) is compiled by the board
// toolchain, where it can `#include <Arduino.h>` and forward to the C++ core:
//
//     extern "C" void ard_pinMode(int pin, int mode) { pinMode(pin, mode); }
//     extern "C" void ard_serial_println(const char *s) { Serial.println(s); }
//
// It exists because the Arduino core is C++ — `Serial` is a global object,
// `String` is a class, the HAL leans on overloading — while Jeti emits C99.
// A C superset reaches a C++ library the same way the soma kernel reaches its
// console: through plain C entry points.
//
// On the host the same signatures are stubbed with stdio so the sketch's Jeti
// side can be compiled and run for real.

#include <stdio.h>
#include <string.h>

static int g_pin_mode[64];
static int g_pin_value[64];
static unsigned g_ms;

void ard_pinMode(int pin, int mode) {
    if (pin >= 0 && pin < 64) g_pin_mode[pin] = mode;
    printf("      [bridge] pinMode(%d, %s)\n", pin, mode ? "OUTPUT" : "INPUT");
}

void ard_digitalWrite(int pin, int value) {
    if (pin >= 0 && pin < 64) g_pin_value[pin] = value;
    printf("      [bridge] digitalWrite(%d, %s)\n", pin, value ? "HIGH" : "LOW");
}

int ard_digitalRead(int pin) {
    // A deterministic host stand-in: the "button" reads high every third poll.
    if (pin >= 0 && pin < 64) return (g_ms / 10) % 3 == 0;
    return 0;
}

unsigned ard_millis(void) { return g_ms; }

void ard_serial_begin(long baud) { printf("      [bridge] Serial.begin(%ld)\n", baud); }

void ard_serial_println(const char *s) { printf("[serial] %s\n", s); }

void ard_serial_printInt(int v) { printf("[serial] %d", v); }

void ard_delay(unsigned ms) { g_ms += ms; }

// ── OLED emulation: the C side of a C++ display library ────────────────────
// A real sketch links Adafruit_SSD1306 (C++) through extern "C" shims; the host
// build stands in for the panel with a 4x22 character-cell frame buffer, so
// oled_status.jeti runs (and prints) without hardware.
#define OLED_ROWS 4
#define OLED_COLS 22

static char g_oled[OLED_ROWS][OLED_COLS + 2];

void oled_begin(void) {
    printf("      [oled] SSD1306 128x64 ready\n");
}

void oled_clear(void) {
    for (int r = 0; r < OLED_ROWS; r++) g_oled[r][0] = 0;
}

void oled_text(int row, int col, const char *s) {
    if (row < 0 || row >= OLED_ROWS || col < 0 || col >= OLED_COLS) return;
    snprintf(g_oled[row] + col, (size_t)(OLED_COLS - col + 1), "%s", s);
}

void oled_value(int row, const char *label, int v) {
    if (row < 0 || row >= OLED_ROWS) return;
    snprintf(g_oled[row], sizeof(g_oled[row]), "%s=%d", label, v);
}

void oled_flush(void) {
    printf("      [oled] +----------------------+\n");
    for (int r = 0; r < OLED_ROWS; r++) printf("      [oled] |%s\n", g_oled[r]);
    printf("      [oled] +----------------------+\n");
}

// The Arduino core owns `main`: it calls setup() once and then loop() forever.
// The host stand-in does the same, for a bounded number of iterations.
extern void setup(void);
extern void loop(void);

int main(void) {
    setup();
    for (int i = 0; i < 3; i++) loop();
    printf("      [bridge] 3 loop() iterations done, t=%ums\n", ard_millis());
    return 0;
}

// ── Protocol conformance ───────────────────────────────────────────────────
// Supplied by include/jeti/runtime_freestanding.c (same metadata-based walk as
// the hosted runtime), so a board port no longer has to carry its own copy.
