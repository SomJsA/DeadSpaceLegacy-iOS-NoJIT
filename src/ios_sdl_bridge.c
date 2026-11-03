/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
#include "SDL.h"
#include "SDL_syswm.h"

static int drawable_info(SDL_Window *window, SDL_SysWMinfo *info) {
    SDL_VERSION(&info->version);
    if (!window || !SDL_GetWindowWMInfo(window, info)) return -1;
    if (info->subsystem != SDL_SYSWM_UIKIT)
        return SDL_SetError("Expected SDL UIKit window system");
    return 0;
}

int touchhle_ios_default_framebuffer(unsigned int *framebuffer) {
    SDL_SysWMinfo info;
    if (drawable_info(SDL_GL_GetCurrentWindow(), &info) != 0) return -1;
    *framebuffer = info.info.uikit.framebuffer;
    return 0;
}

int touchhle_ios_swap_window(SDL_Window *window) {
    SDL_SysWMinfo info;
    if (drawable_info(window, &info) != 0) return -1;
    typedef void (*GetInteger)(unsigned int, int *);
    typedef void (*BindRenderbuffer)(unsigned int, unsigned int);
    GetInteger get_integer = (GetInteger)SDL_GL_GetProcAddress("glGetIntegerv");
    BindRenderbuffer bind_renderbuffer = (BindRenderbuffer)SDL_GL_GetProcAddress("glBindRenderbufferOES");
    if (!bind_renderbuffer)
        bind_renderbuffer = (BindRenderbuffer)SDL_GL_GetProcAddress("glBindRenderbuffer");
    if (!get_integer || !bind_renderbuffer)
        return SDL_SetError("UIKit renderbuffer functions unavailable");
    int old_renderbuffer = 0;
    get_integer(0x8ca7, &old_renderbuffer); /* GL_RENDERBUFFER_BINDING[_OES] */
    bind_renderbuffer(0x8d41, info.info.uikit.colorbuffer);
    SDL_GL_SwapWindow(window);
    bind_renderbuffer(0x8d41, (unsigned int)old_renderbuffer);
    return 0;
}
