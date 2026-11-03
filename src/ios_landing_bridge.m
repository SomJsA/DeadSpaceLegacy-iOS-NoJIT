/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
#import <UIKit/UIKit.h>
#include "SDL.h"
#include "SDL_syswm.h"
#include <stdbool.h>
#include <stdio.h>

bool touchhle_ios_landing_label(void *raw, const char *text) {
    SDL_SysWMinfo info;
    SDL_VERSION(&info.version);
    if (!SDL_GetWindowWMInfo((SDL_Window *)raw, &info) || info.subsystem != SDL_SYSWM_UIKIT)
        return false;
    UIWindow *window = info.info.uikit.window;
    UIView *view = window.rootViewController.view;
    if (!view) return false;
    @autoreleasepool {
        UILabel *label = [[UILabel alloc] initWithFrame:CGRectInset(view.bounds, 32, 32)];
        label.autoresizingMask = UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight;
        label.numberOfLines = 0;
        label.textAlignment = NSTextAlignmentCenter;
        label.textColor = [UIColor whiteColor];
        label.backgroundColor = [UIColor clearColor];
        label.font = [UIFont systemFontOfSize:25 weight:UIFontWeightMedium];
        label.text = [NSString stringWithUTF8String:text];
        [view addSubview:label];
        [label release];
    }
    return true;
}

// SDL 2.26 gives every GL context its own UIKit view. Its MakeCurrent keeps
// the old attachment when that context already belongs to this SDL window.
// Presenting the compositor must restore that context's view before querying
// the window framebuffer, rather than drawing to the last-created guest view.
@protocol TouchHLESDLContextView <NSObject>
- (id)sdlView;
@end
@protocol TouchHLESDLWindowView <NSObject>
- (void)setSDLWindow:(SDL_Window *)window;
@end

int touchhle_ios_activate_current_view(SDL_Window *window) {
    @autoreleasepool {
        id context = (id)SDL_GL_GetCurrentContext();
        if (!context || ![context respondsToSelector:@selector(sdlView)])
            return SDL_SetError("Current UIKit GL context has no SDL view");
        UIView *view = [(id<TouchHLESDLContextView>)context sdlView];
        SDL_SysWMinfo info;
        SDL_VERSION(&info.version);
        if (!window || !SDL_GetWindowWMInfo(window, &info) || info.subsystem != SDL_SYSWM_UIKIT)
            return SDL_SetError("Current GL view requires its UIKit SDL window");
        if (!view || ![view respondsToSelector:@selector(setSDLWindow:)])
            return SDL_SetError("Current UIKit drawable is not an SDL window view");
        if (info.info.uikit.window.rootViewController.view == view)
            return 0;
        // Keep the offscreen view alive while SDL removes/reinserts its owner.
        [view retain];
        [(id<TouchHLESDLWindowView>)view setSDLWindow:NULL];
        [(id<TouchHLESDLWindowView>)view setSDLWindow:window];
        [view release];
        if (info.info.uikit.window.rootViewController.view != view)
            return SDL_SetError("SDL failed to restore the current GL drawable view");
        fprintf(stderr, "[IOSHost] restored active SDL GL view before window presentation\n");
        return 0;
    }
}
