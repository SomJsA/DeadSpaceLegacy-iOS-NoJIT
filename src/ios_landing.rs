/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
//! Mobile setup screen: SDL owns the sole UIKit loop; no desktop app picker.
use crate::gles::gles11_raw as gl;
use std::ffi::{c_char, c_void, CString};

extern "C" {
    fn touchhle_ios_landing_label(window: *mut c_void, text: *const c_char) -> bool;
}

pub fn show(message: &str) -> Result<(), String> {
    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    video.gl_attr().set_context_profile(sdl2::video::GLProfile::GLES);
    video.gl_attr().set_context_version(1, 1);
    sdl2::hint::set("SDL_TOUCH_MOUSE_EVENTS", "0");
    sdl2::hint::set("SDL_MOUSE_TOUCH_EVENTS", "0");
    let window = video.window("Dead Space No-JIT", 768, 1024)
        .fullscreen().opengl().build().map_err(|e| e.to_string())?;
    let context = window.gl_create_context()?;
    window.gl_make_current(&context)?;
    gl::load_with(|name| video.gl_get_proc_address(name).cast());
    let message = CString::new(message).map_err(|e| e.to_string())?;
    if !unsafe { touchhle_ios_landing_label(window.raw().cast(), message.as_ptr()) } {
        return Err("Could not attach the mobile setup view".into());
    }
    eprintln!("[IOSHost] mobile-landing-ready; Android baseline; desktop mappings disabled");
    let mut events = sdl.event_pump()?;
    let mut dirty = true;
    loop {
        for event in events.poll_iter() {
            match event {
                sdl2::event::Event::Quit { .. } => return Ok(()),
                sdl2::event::Event::Window { .. }
                | sdl2::event::Event::AppDidEnterForeground { .. } => dirty = true,
                _ => (),
            }
        }
        if dirty {
            let (width, height) = window.drawable_size();
            unsafe {
                gl::BindFramebufferOES(gl::FRAMEBUFFER_OES, crate::ios_sdl::default_framebuffer());
                gl::Viewport(0, 0, width as i32, height as i32);
                gl::ClearColor(0.025, 0.06, 0.11, 1.0);
                gl::Clear(gl::COLOR_BUFFER_BIT);
            }
            crate::ios_sdl::swap_window(window.raw());
            dirty = false;
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
