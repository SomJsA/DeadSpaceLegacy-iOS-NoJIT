/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
#import <UIKit/UIKit.h>
#include "SDL.h"
#include "SDL_syswm.h"
#include <stdbool.h>

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
