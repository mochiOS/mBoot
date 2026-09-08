/* Runs the production drawing functions in a headless EGL context; no KMS. */
#define main mdriver_program_main
#include "../mdriver-gpu.c"
#undef main
#include <assert.h>

enum { W = 64, H = 48, VERTICES = 18, BATCHES = 3,
       BATCH_OFFSET = SCENE_HEADER_LEN + TEXTURE_DESC_LEN,
       VERTEX_OFFSET = BATCH_OFFSET + BATCHES * BATCH_DESC_LEN,
       DATA_OFFSET = VERTEX_OFFSET + VERTICES * VERTEX_STRIDE,
       PACKET_SIZE = DATA_OFFSET + 4 };

static void put32(uint8_t *p, uint32_t value) { memcpy(p, &value, 4); }
static void put64(uint8_t *p, uint64_t value) { memcpy(p, &value, 8); }
static void quad(uint8_t *p, int x, int y, int w, int h, const float color[4])
{
    const int corners[6][2] = {{0,0},{1,0},{1,1},{0,0},{1,1},{0,1}};
    for (int i = 0; i < 6; i++) {
        float v[9] = {(float)(x + corners[i][0] * w) / W * 2 - 1,
                      (float)(y + corners[i][1] * h) / H * 2 - 1,
                      0, .5f, .5f, color[0], color[1], color[2], color[3]};
        memcpy(p + i * VERTEX_STRIDE, v, sizeof(v));
    }
}

static void packet(uint8_t bytes[PACKET_SIZE], int x, int y, const int dirty[4])
{
    memset(bytes, 0, PACKET_SIZE);
    put32(bytes, SCENE_MAGIC);
    uint16_t version = SCENE_VERSION, header = SCENE_HEADER_LEN;
    memcpy(bytes + 4, &version, 2); memcpy(bytes + 6, &header, 2);
    const uint32_t fields[] = {PACKET_SIZE, W, H, VERTICES, VERTEX_STRIDE,
                              1, BATCHES, SCENE_HEADER_LEN, BATCH_OFFSET, VERTEX_OFFSET, DATA_OFFSET};
    for (unsigned int i = 0; i < ARRAY_LEN(fields); i++) put32(bytes + 8 + i * 4, fields[i]);
    uint8_t *texture = bytes + SCENE_HEADER_LEN;
    put64(texture, UINT64_MAX - 1);
    put32(texture + 8, 1); put32(texture + 12, 1); put32(texture + 20, 1);
    put32(texture + 24, DATA_OFFSET); put32(texture + 28, 4); put64(texture + 32, 1);
    memset(bytes + DATA_OFFSET, 255, 4);
    for (unsigned int i = 0; i < BATCHES; i++) {
        uint8_t *batch = bytes + BATCH_OFFSET + i * BATCH_DESC_LEN;
        put64(batch, UINT64_MAX - 1); put32(batch + 8, i * 6); put32(batch + 12, 6);
    }
    quad(bytes + VERTEX_OFFSET, dirty[0], dirty[1], dirty[2], dirty[3], (float[]){0,0,1,1});
    quad(bytes + VERTEX_OFFSET + 6 * VERTEX_STRIDE, 0, 0, 8, 8, (float[]){1,0,0,1});
    quad(bytes + VERTEX_OFFSET + 12 * VERTEX_STRIDE, x, y, 8, 8, (float[]){0,.5f,0,.5f});
}

static void read_frame(struct renderer *renderer, const uint8_t *bytes, uint8_t *pixels)
{
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(1, 1, 1, 1); /* Simulate a discarded swapchain buffer. */
    glClear(GL_COLOR_BUFFER_BIT);
    assert(draw_scene(renderer, bytes, PACKET_SIZE) == 0);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    assert(glGetError() == GL_NO_ERROR);
}

int main(void)
{
    EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    assert(eglInitialize(display, NULL, NULL));
    assert(eglBindAPI(EGL_OPENGL_ES_API));
    EGLConfig config;
    EGLint count;
    const EGLint attrs[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE,
        EGL_OPENGL_ES2_BIT, EGL_RED_SIZE,8,EGL_GREEN_SIZE,8,EGL_BLUE_SIZE,8,EGL_ALPHA_SIZE,8,EGL_NONE};
    assert(eglChooseConfig(display, attrs, &config, 1, &count) && count);
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT,
        (EGLint[]){EGL_CONTEXT_CLIENT_VERSION,2,EGL_NONE});
    EGLSurface surface = eglCreatePbufferSurface(display, config,
        (EGLint[]){EGL_WIDTH,W,EGL_HEIGHT,H,EGL_NONE});
    assert(eglMakeCurrent(display, surface, surface, context));
    struct renderer full = {.width=W,.height=H}, dirty = {.width=W,.height=H};
    assert(!initialize_scene_program(&full) && !initialize_scene_program(&dirty));
    uint8_t a[PACKET_SIZE], b[PACKET_SIZE], expected[W*H*4], actual[W*H*4];
    int old_x = 16, old_y = 16;
    for (int frame = 0; frame < 200; frame++) {
        int x = 16 + frame % 24, y = 16 + (frame / 24) % 16;
        int left = x < old_x ? x : old_x, top = y < old_y ? y : old_y;
        int right = (x > old_x ? x : old_x) + 8, bottom = (y > old_y ? y : old_y) + 8;
        packet(a, x, y, (int[]){0,0,W,H});
        packet(b, x, y, (int[]){left,top,right-left,bottom-top});
        read_frame(&full, a, expected);
        read_frame(&dirty, b, actual);
        assert(!memcmp(actual, expected, sizeof(actual)));
        /* Check orientation and channels independently of the full-redraw path. */
        const uint8_t *red = actual + ((H - 2) * W + 2) * 4;
        assert(red[0] == 255 && red[1] == 0 && red[2] == 0);
        assert(actual[0] == 0 && actual[1] == 0 && actual[2] == 255);
        old_x = x; old_y = y;
    }
    float invalid = NAN;
    memcpy(b + VERTEX_OFFSET, &invalid, 4);
    assert(draw_scene(&dirty, b, sizeof(b)) == -EINVAL);
    /* Non-power-of-two display sizes must recover exact pixel boundaries. */
    const uint32_t widths[] = {1366, 1920, 3840, 8192};
    for (unsigned int i = 0; i < ARRAY_LEN(widths); i++) {
        uint32_t width = widths[i];
        for (uint32_t x = 0; x < width; x++) {
            float vertices[6][9] = {{0}};
            vertices[0][0] = (float)x / width * 2 - 1;
            vertices[0][1] = -1;
            vertices[0][8] = 1;
            vertices[2][0] = (float)(x + 1) / width * 2 - 1;
            vertices[2][1] = 1;
            uint32_t rect[4]; GLfloat color[4];
            assert(!replacement_rect((uint8_t *)vertices, width, H, rect, color));
            assert(rect[0] == x && rect[1] == 0 && rect[2] == 1 && rect[3] == H);
        }
    }
    puts("PASS: 200 EGL/GLES dirty frames equal full redraw; initial partial frame, cursor trails, alpha, channels and orientation verified");
    return 0;
}
