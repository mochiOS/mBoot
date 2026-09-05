// SPDX-License-Identifier: Apache-2.0

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <drm_fourcc.h>
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <linux/fb.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#define ARRAY_LEN(a) (sizeof(a) / sizeof((a)[0]))
#define MAX_TEXTURES 64
#define MAX_BATCHES 128
#define MAX_VERTICES 65536
#define VERTEX_STRIDE 36
#define SCENE_MAGIC 0x43474b56u
#define SCENE_VERSION 1
#define SCENE_HEADER_LEN 64
#define TEXTURE_DESC_LEN 40
#define BATCH_DESC_LEN 16

struct gpu_request {
    uint64_t generation;
    uint64_t buffer_size;
    uint32_t scene_length;
    uint32_t reserved;
};

struct gpu_response {
    uint64_t generation;
    int32_t status;
    uint32_t reserved;
};

struct texture {
    uint64_t key;
    uint64_t generation;
    uint32_t width;
    uint32_t height;
    GLuint id;
    int active;
};

struct drm_framebuffer {
    int fd;
    uint32_t id;
};

struct renderer {
    int drm_fd;
    uint32_t connector_id;
    uint32_t crtc_id;
    drmModeModeInfo mode;
    drmModeCrtc *saved_crtc;
    struct gbm_device *gbm;
    struct gbm_surface *surface;
    EGLDisplay egl_display;
    EGLContext egl_context;
    EGLSurface egl_surface;
    GLuint scene_program;
    GLuint copy_program;
    GLuint framebuffer;
    GLuint color_texture;
    GLuint vertex_buffer;
    GLint scene_position;
    GLint scene_uv;
    GLint scene_color;
    GLint scene_sampler;
    GLint copy_position;
    GLint copy_uv;
    GLint copy_sampler;
    struct texture textures[MAX_TEXTURES];
    struct gbm_bo *front_bo;
    uint32_t width;
    uint32_t height;
};

static uint32_t fb_color(const struct fb_var_screeninfo *info,
                         uint8_t red, uint8_t green, uint8_t blue)
{
    uint32_t color = 0;
    if (info->red.length && info->red.length < 32)
        color |= (uint32_t)(((uint64_t)red * ((1ULL << info->red.length) - 1) / 255)
                            << info->red.offset);
    if (info->green.length && info->green.length < 32)
        color |= (uint32_t)(((uint64_t)green * ((1ULL << info->green.length) - 1) / 255)
                            << info->green.offset);
    if (info->blue.length && info->blue.length < 32)
        color |= (uint32_t)(((uint64_t)blue * ((1ULL << info->blue.length) - 1) / 255)
                            << info->blue.offset);
    return color;
}

static void fb_rect(uint8_t *pixels, const struct fb_fix_screeninfo *fixed,
                    const struct fb_var_screeninfo *variable, uint32_t x,
                    uint32_t y, uint32_t width, uint32_t height, uint32_t color)
{
    uint32_t right = x + width < variable->xres ? x + width : variable->xres;
    uint32_t bottom = y + height < variable->yres ? y + height : variable->yres;
    for (uint32_t row = y; row < bottom; row++) {
        uint32_t *destination = (uint32_t *)(pixels +
            (row + variable->yoffset) * fixed->line_length) + x + variable->xoffset;
        for (uint32_t column = x; column < right; column++)
            *destination++ = color;
    }
}

static void fb_hex_digit(uint8_t *pixels, const struct fb_fix_screeninfo *fixed,
                         const struct fb_var_screeninfo *variable, uint32_t x,
                         uint32_t y, uint32_t scale, unsigned int value,
                         uint32_t color)
{
    static const uint8_t segments[16] = {
        0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07,
        0x7f, 0x6f, 0x77, 0x7c, 0x39, 0x5e, 0x79, 0x71,
    };
    uint8_t enabled = segments[value & 0xf];
    if (enabled & 0x01) fb_rect(pixels, fixed, variable, x + scale, y, scale * 3, scale, color);
    if (enabled & 0x02) fb_rect(pixels, fixed, variable, x + scale * 4, y + scale, scale, scale * 2, color);
    if (enabled & 0x04) fb_rect(pixels, fixed, variable, x + scale * 4, y + scale * 4, scale, scale * 2, color);
    if (enabled & 0x08) fb_rect(pixels, fixed, variable, x + scale, y + scale * 6, scale * 3, scale, color);
    if (enabled & 0x10) fb_rect(pixels, fixed, variable, x, y + scale * 4, scale, scale * 2, color);
    if (enabled & 0x20) fb_rect(pixels, fixed, variable, x, y + scale, scale, scale * 2, color);
    if (enabled & 0x40) fb_rect(pixels, fixed, variable, x + scale, y + scale * 3, scale * 3, scale, color);
}

static void report_gpu_failure(unsigned int stage)
{
    struct fb_fix_screeninfo fixed;
    struct fb_var_screeninfo variable;
    int fd = open("/dev/fb0", O_RDWR | O_CLOEXEC);
    if (fd < 0 || ioctl(fd, FBIOGET_FSCREENINFO, &fixed) ||
        ioctl(fd, FBIOGET_VSCREENINFO, &variable) || variable.bits_per_pixel != 32 ||
        !variable.xres || !variable.yres || !fixed.smem_len ||
        (uint64_t)(variable.yoffset + variable.yres) * fixed.line_length > fixed.smem_len) {
        if (fd >= 0)
            close(fd);
        return;
    }
    uint8_t *pixels = mmap(NULL, fixed.smem_len, PROT_READ | PROT_WRITE,
                           MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) {
        close(fd);
        return;
    }
    uint32_t background = fb_color(&variable, 180, 24, 38);
    uint32_t foreground = fb_color(&variable, 255, 255, 255);
    fb_rect(pixels, &fixed, &variable, 0, 0, variable.xres, variable.yres, background);
    uint32_t scale = variable.xres < variable.yres ? variable.xres / 32 : variable.yres / 18;
    if (!scale)
        scale = 1;
    uint32_t total_width = scale * 12;
    uint32_t x = variable.xres > total_width ? (variable.xres - total_width) / 2 : 0;
    uint32_t y = variable.yres > scale * 7 ? (variable.yres - scale * 7) / 2 : 0;
    fb_hex_digit(pixels, &fixed, &variable, x, y, scale, stage >> 4, foreground);
    fb_hex_digit(pixels, &fixed, &variable, x + scale * 7, y, scale, stage, foreground);
    msync(pixels, fixed.smem_len, MS_SYNC);
    munmap(pixels, fixed.smem_len);
    close(fd);
}

static uint16_t get_u16(const uint8_t *p)
{
    return (uint16_t)p[0] | (uint16_t)p[1] << 8;
}

static uint32_t get_u32(const uint8_t *p)
{
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 |
           (uint32_t)p[3] << 24;
}

static uint64_t get_u64(const uint8_t *p)
{
    return (uint64_t)get_u32(p) | (uint64_t)get_u32(p + 4) << 32;
}

static int write_response(int fd, uint64_t generation, int status)
{
    struct gpu_response response = {
        .generation = generation,
        .status = status,
    };
    return write(fd, &response, sizeof(response)) == sizeof(response) ? 0 : -1;
}

static int choose_output(int fd, struct renderer *renderer)
{
    drmModeRes *resources = drmModeGetResources(fd);
    if (!resources)
        return -1;
    for (int ci = 0; ci < resources->count_connectors; ci++) {
        drmModeConnector *connector = drmModeGetConnector(fd, resources->connectors[ci]);
        if (!connector)
            continue;
        if (connector->connection != DRM_MODE_CONNECTED || connector->count_modes == 0) {
            drmModeFreeConnector(connector);
            continue;
        }
        drmModeEncoder *encoder = connector->encoder_id ?
            drmModeGetEncoder(fd, connector->encoder_id) : NULL;
        uint32_t crtc = encoder ? encoder->crtc_id : 0;
        if (!crtc) {
            for (int ei = 0; ei < connector->count_encoders && !crtc; ei++) {
                drmModeEncoder *candidate = drmModeGetEncoder(fd, connector->encoders[ei]);
                if (!candidate)
                    continue;
                for (int ri = 0; ri < resources->count_crtcs; ri++) {
                    if (candidate->possible_crtcs & (1u << ri)) {
                        crtc = resources->crtcs[ri];
                        break;
                    }
                }
                drmModeFreeEncoder(candidate);
            }
        }
        if (encoder)
            drmModeFreeEncoder(encoder);
        if (!crtc) {
            drmModeFreeConnector(connector);
            continue;
        }
        int preferred = 0;
        for (int mi = 0; mi < connector->count_modes; mi++) {
            if (connector->modes[mi].type & DRM_MODE_TYPE_PREFERRED) {
                preferred = mi;
                break;
            }
        }
        renderer->connector_id = connector->connector_id;
        renderer->crtc_id = crtc;
        renderer->saved_crtc = drmModeGetCrtc(fd, crtc);
        renderer->mode = renderer->saved_crtc && renderer->saved_crtc->mode_valid
            ? renderer->saved_crtc->mode
            : connector->modes[preferred];
        drmModeFreeConnector(connector);
        drmModeFreeResources(resources);
        return 0;
    }
    drmModeFreeResources(resources);
    return -1;
}

static int open_drm(struct renderer *renderer, unsigned int *failure_stage)
{
    char path[32];
    int found_card = 0;
    for (unsigned int index = 0; index < 16; index++) {
        snprintf(path, sizeof(path), "/dev/dri/card%u", index);
        int fd = open(path, O_RDWR | O_CLOEXEC);
        if (fd < 0)
            continue;
        found_card = 1;
        if (choose_output(fd, renderer) == 0) {
            renderer->drm_fd = fd;
            return 0;
        }
        close(fd);
    }
    *failure_stage = found_card ? 0x02 : 0x01;
    return -1;
}

static GLuint compile_shader(GLenum type, const char *source)
{
    GLuint shader = glCreateShader(type);
    GLint ok = 0;
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[512];
        glGetShaderInfoLog(shader, sizeof(log), NULL, log);
        dprintf(2, "mDriver GPU: shader: %s\n", log);
        glDeleteShader(shader);
        return 0;
    }
    return shader;
}

static GLuint make_program(const char *vertex_source, const char *fragment_source)
{
    GLuint vertex = compile_shader(GL_VERTEX_SHADER, vertex_source);
    GLuint fragment = compile_shader(GL_FRAGMENT_SHADER, fragment_source);
    if (!vertex || !fragment)
        return 0;
    GLuint program = glCreateProgram();
    GLint ok = 0;
    glAttachShader(program, vertex);
    glAttachShader(program, fragment);
    glLinkProgram(program);
    glGetProgramiv(program, GL_LINK_STATUS, &ok);
    glDeleteShader(vertex);
    glDeleteShader(fragment);
    if (!ok) {
        glDeleteProgram(program);
        return 0;
    }
    return program;
}

static int choose_egl_config(EGLDisplay display, const EGLint *attributes,
                             EGLConfig *selected)
{
    EGLConfig configs[64];
    EGLint count = 0;

    if (!eglChooseConfig(display, attributes, configs, ARRAY_LEN(configs), &count))
        return -1;
    for (EGLint index = 0; index < count; index++) {
        EGLint visual = 0;
        if (eglGetConfigAttrib(display, configs[index], EGL_NATIVE_VISUAL_ID, &visual) &&
            visual == GBM_FORMAT_XRGB8888) {
            *selected = configs[index];
            return 0;
        }
    }
    return -1;
}

static int initialize_gl(struct renderer *renderer)
{
    static const EGLint config_attributes[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8,
        EGL_ALPHA_SIZE, 0, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_NONE,
    };
    static const EGLint context_attributes[] = {
        EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE,
    };
    static const char scene_vertex[] =
        "attribute vec3 position; attribute vec2 uv; attribute vec4 color;"
        "varying vec2 v_uv; varying vec4 v_color;"
        "void main(){gl_Position=vec4(position.x,-position.y,position.z,1.0);"
        "v_uv=uv;v_color=color;}";
    static const char scene_fragment[] =
        "precision mediump float; varying vec2 v_uv; varying vec4 v_color;"
        "uniform sampler2D image; void main(){vec4 p=texture2D(image,v_uv);"
        "gl_FragColor=vec4(p.b,p.g,p.r,p.a)*v_color;}";
    static const char copy_vertex[] =
        "attribute vec2 position; attribute vec2 uv; varying vec2 v_uv;"
        "void main(){gl_Position=vec4(position,0.0,1.0);v_uv=uv;}";
    static const char copy_fragment[] =
        "precision mediump float; varying vec2 v_uv; uniform sampler2D image;"
        "void main(){gl_FragColor=texture2D(image,v_uv);}";
    EGLConfig config;

    renderer->gbm = gbm_create_device(renderer->drm_fd);
    if (!renderer->gbm)
        return 0x03;
    renderer->surface = gbm_surface_create(renderer->gbm, renderer->mode.hdisplay,
        renderer->mode.vdisplay, GBM_FORMAT_XRGB8888,
        GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING);
    if (!renderer->surface)
        return 0x04;
    renderer->egl_display = eglGetDisplay((EGLNativeDisplayType)renderer->gbm);
    if (renderer->egl_display == EGL_NO_DISPLAY)
        return 0x05;
    if (!eglInitialize(renderer->egl_display, NULL, NULL))
        return 0x06;
    if (!eglBindAPI(EGL_OPENGL_ES_API))
        return 0x07;
    if (choose_egl_config(renderer->egl_display, config_attributes, &config))
        return 0x08;
    renderer->egl_context = eglCreateContext(renderer->egl_display, config,
        EGL_NO_CONTEXT, context_attributes);
    if (renderer->egl_context == EGL_NO_CONTEXT)
        return 0x09;
    renderer->egl_surface = eglCreateWindowSurface(renderer->egl_display, config,
        (EGLNativeWindowType)renderer->surface, NULL);
    if (renderer->egl_surface == EGL_NO_SURFACE)
        return 0x0a;
    if (!eglMakeCurrent(renderer->egl_display, renderer->egl_surface,
                        renderer->egl_surface, renderer->egl_context))
        return 0x0b;
    const char *gpu_name = (const char *)glGetString(GL_RENDERER);
    if (!gpu_name || strstr(gpu_name, "llvmpipe") || strstr(gpu_name, "softpipe") ||
        strstr(gpu_name, "Software Rasterizer")) {
        dprintf(2, "mDriver GPU: refusing software renderer %s\n",
                gpu_name ? gpu_name : "unknown");
        return 0x0c;
    }
    dprintf(2, "mDriver GPU: renderer=%s\n", gpu_name);

    renderer->scene_program = make_program(scene_vertex, scene_fragment);
    renderer->copy_program = make_program(copy_vertex, copy_fragment);
    if (!renderer->scene_program || !renderer->copy_program)
        return 0x0d;
    renderer->scene_position = glGetAttribLocation(renderer->scene_program, "position");
    renderer->scene_uv = glGetAttribLocation(renderer->scene_program, "uv");
    renderer->scene_color = glGetAttribLocation(renderer->scene_program, "color");
    renderer->scene_sampler = glGetUniformLocation(renderer->scene_program, "image");
    renderer->copy_position = glGetAttribLocation(renderer->copy_program, "position");
    renderer->copy_uv = glGetAttribLocation(renderer->copy_program, "uv");
    renderer->copy_sampler = glGetUniformLocation(renderer->copy_program, "image");
    glGenBuffers(1, &renderer->vertex_buffer);
    glGenFramebuffers(1, &renderer->framebuffer);
    glGenTextures(1, &renderer->color_texture);
    glBindTexture(GL_TEXTURE_2D, renderer->color_texture);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, renderer->mode.hdisplay,
                 renderer->mode.vdisplay, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glBindFramebuffer(GL_FRAMEBUFFER, renderer->framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                           renderer->color_texture, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE)
        return 0x0e;
    renderer->width = renderer->mode.hdisplay;
    renderer->height = renderer->mode.vdisplay;
    /* KMS page flips below provide the single frame-rate boundary. */
    eglSwapInterval(renderer->egl_display, 0);
    return 0;
}

static struct texture *find_texture(struct renderer *renderer, uint64_t key)
{
    for (size_t index = 0; index < ARRAY_LEN(renderer->textures); index++)
        if (renderer->textures[index].active && renderer->textures[index].key == key)
            return &renderer->textures[index];
    return NULL;
}

static struct texture *allocate_texture(struct renderer *renderer, uint64_t key)
{
    struct texture *texture = find_texture(renderer, key);
    if (texture)
        return texture;
    for (size_t index = 0; index < ARRAY_LEN(renderer->textures); index++) {
        if (!renderer->textures[index].active) {
            texture = &renderer->textures[index];
            memset(texture, 0, sizeof(*texture));
            texture->active = 1;
            texture->key = key;
            glGenTextures(1, &texture->id);
            return texture;
        }
    }
    return NULL;
}

static int sync_textures(struct renderer *renderer, const uint8_t *scene, size_t length,
                         uint32_t texture_count)
{
    for (size_t index = 0; index < ARRAY_LEN(renderer->textures); index++)
        renderer->textures[index].active = renderer->textures[index].active ? 2 : 0;
    for (uint32_t index = 0; index < texture_count; index++) {
        const uint8_t *desc = scene + SCENE_HEADER_LEN + index * TEXTURE_DESC_LEN;
        uint64_t key = get_u64(desc);
        uint32_t width = get_u32(desc + 8), height = get_u32(desc + 12);
        uint32_t data_y = get_u32(desc + 16), data_height = get_u32(desc + 20);
        uint32_t data_offset = get_u32(desc + 24), data_length = get_u32(desc + 28);
        uint64_t generation = get_u64(desc + 32);
        if (!width || !height || width > 8192 || height > 8192 ||
            data_y > height || data_height > height - data_y ||
            (uint64_t)width * data_height * 4 != data_length ||
            (data_length && ((uint64_t)data_offset + data_length > length)))
            return -EINVAL;
        struct texture *texture = find_texture(renderer, key);
        if (!texture)
            texture = allocate_texture(renderer, key);
        if (!texture)
            return -ENOSPC;
        texture->active = 1;
        glBindTexture(GL_TEXTURE_2D, texture->id);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        if (texture->width != width || texture->height != height) {
            if (data_y != 0 || data_height != height)
                return -EINVAL;
            glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, width, height, 0,
                         GL_RGBA, GL_UNSIGNED_BYTE, scene + data_offset);
            texture->width = width;
            texture->height = height;
        } else if (data_length) {
            glTexSubImage2D(GL_TEXTURE_2D, 0, 0, data_y, width, data_height,
                            GL_RGBA, GL_UNSIGNED_BYTE, scene + data_offset);
        } else if (texture->generation != generation) {
            return -EINVAL;
        }
        texture->generation = generation;
    }
    for (size_t index = 0; index < ARRAY_LEN(renderer->textures); index++) {
        if (renderer->textures[index].active == 2) {
            glDeleteTextures(1, &renderer->textures[index].id);
            memset(&renderer->textures[index], 0, sizeof(renderer->textures[index]));
        }
    }
    return glGetError() == GL_NO_ERROR ? 0 : -EIO;
}

static void page_flip_complete(int fd, unsigned int sequence, unsigned int seconds,
                               unsigned int microseconds, void *data)
{
    (void)fd;
    (void)sequence;
    (void)seconds;
    (void)microseconds;
    *(int *)data = 0;
}

static int wait_for_page_flip(int fd, int *waiting)
{
    drmEventContext context = {
        .version = DRM_EVENT_CONTEXT_VERSION,
        .page_flip_handler = page_flip_complete,
    };
    while (*waiting) {
        struct pollfd poll_fd = { .fd = fd, .events = POLLIN };
        int result = poll(&poll_fd, 1, 1000);
        if (result <= 0)
            return result ? -errno : -ETIMEDOUT;
        if (drmHandleEvent(fd, &context))
            return -errno;
    }
    return 0;
}

static void destroy_drm_framebuffer(struct gbm_bo *bo, void *data)
{
    (void)bo;
    struct drm_framebuffer *framebuffer = data;
    if (!framebuffer)
        return;
    drmModeRmFB(framebuffer->fd, framebuffer->id);
    free(framebuffer);
}

static uint32_t framebuffer_for_bo(struct renderer *renderer, struct gbm_bo *bo)
{
    struct drm_framebuffer *framebuffer = gbm_bo_get_user_data(bo);
    if (framebuffer)
        return framebuffer->id;

    uint32_t handles[4] = { 0 };
    uint32_t pitches[4] = { 0 };
    uint32_t offsets[4] = { 0 };
    uint64_t modifiers[4] = { 0 };
    int plane_count = gbm_bo_get_plane_count(bo);
    uint64_t modifier = gbm_bo_get_modifier(bo);
    if (plane_count < 1 || plane_count > (int)ARRAY_LEN(handles)) {
        errno = EINVAL;
        return 0;
    }
    for (int plane = 0; plane < plane_count; plane++) {
        handles[plane] = gbm_bo_get_handle_for_plane(bo, plane).u32;
        pitches[plane] = gbm_bo_get_stride_for_plane(bo, plane);
        offsets[plane] = gbm_bo_get_offset(bo, plane);
        modifiers[plane] = modifier;
    }

    framebuffer = calloc(1, sizeof(*framebuffer));
    if (!framebuffer)
        return 0;
    framebuffer->fd = renderer->drm_fd;
    int result;
    if (modifier == DRM_FORMAT_MOD_INVALID) {
        result = drmModeAddFB2(renderer->drm_fd, renderer->width,
                               renderer->height, gbm_bo_get_format(bo), handles,
                               pitches, offsets, &framebuffer->id, 0);
    } else {
        result = drmModeAddFB2WithModifiers(renderer->drm_fd, renderer->width,
                                             renderer->height,
                                             gbm_bo_get_format(bo), handles,
                                             pitches, offsets, modifiers,
                                             &framebuffer->id,
                                             DRM_MODE_FB_MODIFIERS);
        if (result && modifier == DRM_FORMAT_MOD_LINEAR)
            result = drmModeAddFB2(renderer->drm_fd, renderer->width,
                                   renderer->height, gbm_bo_get_format(bo),
                                   handles, pitches, offsets,
                                   &framebuffer->id, 0);
    }
    if (result) {
        free(framebuffer);
        return 0;
    }
    gbm_bo_set_user_data(bo, framebuffer, destroy_drm_framebuffer);
    return framebuffer->id;
}

static int show_frame(struct renderer *renderer)
{
    /* Do not hand KMS a buffer until rendering into it has completed. */
    glFinish();
    if (glGetError() != GL_NO_ERROR)
        return -EIO;
    if (!eglSwapBuffers(renderer->egl_display, renderer->egl_surface))
        return -EIO;
    struct gbm_bo *bo = gbm_surface_lock_front_buffer(renderer->surface);
    if (!bo)
        return -EIO;
    uint32_t fb = framebuffer_for_bo(renderer, bo);
    if (!fb) {
        gbm_surface_release_buffer(renderer->surface, bo);
        return errno ? -errno : -EIO;
    }
    int result;
    if (!renderer->front_bo) {
        result = drmModeSetCrtc(renderer->drm_fd, renderer->crtc_id, fb, 0, 0,
                                &renderer->connector_id, 1, &renderer->mode);
    } else {
        int waiting = 1;
        result = drmModePageFlip(renderer->drm_fd, renderer->crtc_id, fb,
                                 DRM_MODE_PAGE_FLIP_EVENT, &waiting);
        if (!result)
            result = wait_for_page_flip(renderer->drm_fd, &waiting);
        if (result)
            result = drmModeSetCrtc(renderer->drm_fd, renderer->crtc_id, fb, 0, 0,
                                    &renderer->connector_id, 1, &renderer->mode);
    }
    if (result) {
        gbm_surface_release_buffer(renderer->surface, bo);
        return -errno;
    }
    if (renderer->front_bo) {
        gbm_surface_release_buffer(renderer->surface, renderer->front_bo);
    }
    renderer->front_bo = bo;
    return 0;
}

static int show_startup_frame(struct renderer *renderer)
{
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glViewport(0, 0, renderer->width, renderer->height);
    glDisable(GL_BLEND);
    glClearColor(200.0f / 255.0f, 200.0f / 255.0f, 200.0f / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (glGetError() != GL_NO_ERROR)
        return -EIO;
    return show_frame(renderer);
}

static int render_scene(struct renderer *renderer, const uint8_t *scene, size_t length)
{
    if (length < SCENE_HEADER_LEN || get_u32(scene) != SCENE_MAGIC ||
        get_u16(scene + 4) != SCENE_VERSION || get_u16(scene + 6) != SCENE_HEADER_LEN ||
        get_u32(scene + 8) != length || get_u32(scene + 12) != renderer->width ||
        get_u32(scene + 16) != renderer->height || get_u32(scene + 24) != VERTEX_STRIDE)
        return -EINVAL;
    uint32_t vertex_count = get_u32(scene + 20);
    uint32_t texture_count = get_u32(scene + 28);
    uint32_t batch_count = get_u32(scene + 32);
    uint32_t texture_offset = get_u32(scene + 36);
    uint32_t batch_offset = get_u32(scene + 40);
    uint32_t vertex_offset = get_u32(scene + 44);
    uint32_t data_offset = get_u32(scene + 48);
    uint64_t expected_batch = SCENE_HEADER_LEN + (uint64_t)texture_count * TEXTURE_DESC_LEN;
    uint64_t expected_vertex = expected_batch + (uint64_t)batch_count * BATCH_DESC_LEN;
    uint64_t expected_data = expected_vertex + (uint64_t)vertex_count * VERTEX_STRIDE;
    if (!vertex_count || vertex_count > MAX_VERTICES || vertex_count % 3 ||
        !texture_count || texture_count > MAX_TEXTURES ||
        !batch_count || batch_count > MAX_BATCHES ||
        texture_offset != SCENE_HEADER_LEN || batch_offset != expected_batch ||
        vertex_offset != expected_vertex || data_offset != expected_data || data_offset > length)
        return -EINVAL;
    int result = sync_textures(renderer, scene, length, texture_count);
    if (result)
        return result;

    glBindFramebuffer(GL_FRAMEBUFFER, renderer->framebuffer);
    glViewport(0, 0, renderer->width, renderer->height);
    glUseProgram(renderer->scene_program);
    glBindBuffer(GL_ARRAY_BUFFER, renderer->vertex_buffer);
    glBufferData(GL_ARRAY_BUFFER, (size_t)vertex_count * VERTEX_STRIDE,
                 scene + vertex_offset, GL_STREAM_DRAW);
    glEnableVertexAttribArray(renderer->scene_position);
    glEnableVertexAttribArray(renderer->scene_uv);
    glEnableVertexAttribArray(renderer->scene_color);
    glVertexAttribPointer(renderer->scene_position, 3, GL_FLOAT, GL_FALSE,
                          VERTEX_STRIDE, (void *)0);
    glVertexAttribPointer(renderer->scene_uv, 2, GL_FLOAT, GL_FALSE,
                          VERTEX_STRIDE, (void *)12);
    glVertexAttribPointer(renderer->scene_color, 4, GL_FLOAT, GL_FALSE,
                          VERTEX_STRIDE, (void *)20);
    glUniform1i(renderer->scene_sampler, 0);
    for (uint32_t index = 0; index < batch_count; index++) {
        const uint8_t *batch = scene + batch_offset + index * BATCH_DESC_LEN;
        uint64_t key = get_u64(batch);
        uint32_t first = get_u32(batch + 8), count = get_u32(batch + 12);
        struct texture *texture = find_texture(renderer, key);
        if (!texture || !count || count % 3 || first > vertex_count || count > vertex_count - first)
            return -EINVAL;
        if (index == 0)
            glDisable(GL_BLEND);
        else {
            glEnable(GL_BLEND);
            glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        }
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, texture->id);
        glDrawArrays(GL_TRIANGLES, first, count);
    }

    static const GLfloat quad[] = {
        -1, -1, 0, 0,  1, -1, 1, 0,  1, 1, 1, 1,
        -1, -1, 0, 0,  1, 1, 1, 1, -1, 1, 0, 1,
    };
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDisable(GL_BLEND);
    glUseProgram(renderer->copy_program);
    glBindBuffer(GL_ARRAY_BUFFER, 0);
    glEnableVertexAttribArray(renderer->copy_position);
    glEnableVertexAttribArray(renderer->copy_uv);
    glVertexAttribPointer(renderer->copy_position, 2, GL_FLOAT, GL_FALSE,
                          4 * sizeof(GLfloat), quad);
    glVertexAttribPointer(renderer->copy_uv, 2, GL_FLOAT, GL_FALSE,
                          4 * sizeof(GLfloat), quad + 2);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, renderer->color_texture);
    glUniform1i(renderer->copy_sampler, 0);
    glDrawArrays(GL_TRIANGLES, 0, 6);
    if (glGetError() != GL_NO_ERROR)
        return -EIO;
    return show_frame(renderer);
}

int main(void)
{
    struct renderer renderer = { .drm_fd = -1 };
    unsigned int failure_stage = 0x01;
    unsigned int wait_cycles = 0;
    while (open_drm(&renderer, &failure_stage)) {
        if (++wait_cycles == 50)
            report_gpu_failure(failure_stage);
        poll(NULL, 0, 100);
    }
    failure_stage = initialize_gl(&renderer);
    if (failure_stage) {
        dprintf(2, "mDriver GPU: initialization failed stage=%02x errno=%d\n",
                failure_stage, errno);
        report_gpu_failure(failure_stage);
        return 1;
    }
    if (show_startup_frame(&renderer)) {
        dprintf(2, "mDriver GPU: startup frame failed errno=%d\n", errno);
        report_gpu_failure(0x0f);
        return 1;
    }
    int control = open("/dev/mboot-gpu", O_RDWR | O_CLOEXEC);
    if (control < 0) {
        dprintf(2, "mDriver GPU: control unavailable errno=%d\n", errno);
        return 1;
    }
    dprintf(2, "mDriver GPU: hardware renderer ready\n");
    void *mapping = MAP_FAILED;
    size_t mapped_size = 0;
    for (;;) {
        struct gpu_request request;
        ssize_t received = read(control, &request, sizeof(request));
        if (received != sizeof(request)) {
            if (received < 0 && errno == EINTR)
                continue;
            break;
        }
        if (request.reserved || !request.scene_length ||
            request.scene_length > request.buffer_size) {
            write_response(control, request.generation, -EINVAL);
            continue;
        }
        if (mapping == MAP_FAILED || mapped_size != request.buffer_size) {
            if (mapping != MAP_FAILED)
                munmap(mapping, mapped_size);
            mapped_size = request.buffer_size;
            mapping = mmap(NULL, mapped_size, PROT_READ | PROT_WRITE, MAP_SHARED, control, 0);
        }
        int status = mapping == MAP_FAILED ? -errno :
            render_scene(&renderer, mapping, request.scene_length);
        if (write_response(control, request.generation, status))
            break;
    }
    return 1;
}
