/* SPDX-License-Identifier: Apache-2.0 */
#include <time.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

enum { TIMING_GAP, TIMING_DRAW, TIMING_SWAP, TIMING_GBM, TIMING_WAIT, TIMING_KMS, TIMING_CURSOR, TIMING_MAKE, TIMING_IPC, TIMING_CONTROL, TIMING_COUNT };
#define TIMING_WIDTH 128
#define TIMING_HEIGHT (4 + 9 * (TIMING_COUNT + 2))

struct frame_timing {
    int enabled;
    uint64_t frames;
    uint64_t completed_at;
    uint64_t last[TIMING_COUNT];
    uint64_t maximum[TIMING_COUNT];
};

static uint64_t timing_now(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now))
        return 0;
    return (uint64_t)now.tv_sec * 1000000000 + now.tv_nsec;
}

static uint64_t timing_elapsed(uint64_t start)
{
    uint64_t end = timing_now();
    return start && end >= start ? end - start : 0;
}

static uint64_t timing_lap(const struct frame_timing *timing, uint64_t *start)
{
    if (!timing->enabled)
        return 0;
    uint64_t end = timing_now();
    uint64_t elapsed = *start && end >= *start ? end - *start : 0;
    *start = end;
    return elapsed;
}

static void timing_sample(struct frame_timing *timing, unsigned int index, uint64_t value)
{
    timing->last[index] = value;
    if (value > timing->maximum[index])
        timing->maximum[index] = value;
}

static void timing_record(struct frame_timing *timing, const uint64_t *values)
{
    for (unsigned int i = 0; i < TIMING_COUNT; i++)
        timing_sample(timing, i, values[i]);
    timing->frames++;
}

static void timing_text(uint32_t *pixels, unsigned int row, const char *text)
{
    /* Five-column bitmap glyphs, rendered as one GPU texture below. */
    static const char alphabet[] = "0123456789ADEGKMNPRSUVWXBITCL";
    static const uint8_t glyphs[][5] = {
        {62,81,73,69,62}, {0,66,127,64,0}, {66,97,81,73,70},
        {33,65,69,75,49}, {24,20,18,127,16}, {39,69,69,69,57},
        {60,74,73,73,48}, {1,113,9,5,3}, {54,73,73,73,54},
        {6,73,73,41,30}, {126,17,17,17,126}, {127,65,65,34,28},
        {127,73,73,73,65}, {62,65,73,73,122}, {127,8,20,34,65},
        {127,2,12,2,127}, {127,4,8,16,127}, {127,9,9,9,6},
        {127,9,25,41,70}, {70,73,73,73,49}, {63,64,64,64,63},
        {31,32,64,32,31}, {63,64,56,64,63}, {99,20,8,20,99},
        {127,73,73,73,54}, {0,65,127,65,0}, {1,1,127,1,1},
        {62,65,65,65,34},
        {127,64,64,64,64},
    };
    for (unsigned int n = 0; text[n] && n < 20; n++) {
        const char *glyph = strchr(alphabet, text[n]);
        if (!glyph)
            continue;
        for (unsigned int x = 0; x < 5; x++)
            for (unsigned int y = 0; y < 7; y++)
                if (glyphs[glyph - alphabet][x] & (1u << y))
                    pixels[(4 + row * 9 + y) * TIMING_WIDTH + 4 + n * 6 + x] = 0xffffffff;
    }
}

static void timing_pixels(const struct frame_timing *timing, uint32_t *pixels)
{
    static const char *labels[TIMING_COUNT] = { "GAP ", "DRAW", "SWAP", "GBM ", "WAIT", "KMS ", "CURS", "MAKE", "IPC ", "CTRL" };
    char line[32];
    for (unsigned int i = 0; i < TIMING_WIDTH * TIMING_HEIGHT; i++)
        pixels[i] = 0xff202020;
    timing_text(pixels, 0, "PREV MS       MAX MS");
    for (unsigned int i = 0; i < TIMING_COUNT; i++) {
        uint64_t last = timing->last[i] / 1000000;
        uint64_t maximum = timing->maximum[i] / 1000000;
        snprintf(line, sizeof(line), "%s %5llu     %5llu", labels[i],
                 (unsigned long long)(last > 99999 ? 99999 : last),
                 (unsigned long long)(maximum > 99999 ? 99999 : maximum));
        timing_text(pixels, i + 1, line);
    }
    snprintf(line, sizeof(line), "N %llu", (unsigned long long)timing->frames);
    timing_text(pixels, TIMING_COUNT + 1, line);
}
