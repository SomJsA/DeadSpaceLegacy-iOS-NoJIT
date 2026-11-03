/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
//! UIKit drawables use nonzero framebuffers and a particular color renderbuffer.
extern "C" {
    fn touchhle_ios_default_framebuffer(framebuffer: *mut u32) -> i32;
    fn touchhle_ios_swap_window(window: *mut sdl2_sys::SDL_Window) -> i32;
}
pub(crate) fn default_framebuffer() -> u32 {
    let mut value = 0;
    assert_eq!(
        unsafe { touchhle_ios_default_framebuffer(&mut value) },
        0,
        "UIKit framebuffer: {}",
        sdl2::get_error()
    );
    value
}
pub(crate) fn swap_window(window: *mut sdl2_sys::SDL_Window) {
    assert_eq!(
        unsafe { touchhle_ios_swap_window(window) },
        0,
        "UIKit swap: {}",
        sdl2::get_error()
    );
}
