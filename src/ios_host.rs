/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
//! SDL owns UIKit and the main thread; the ordinary HLE entry runs inside it.
use std::ffi::{c_char, c_void, CStr};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

#[link(name = "Foundation", kind = "framework")]
extern "C" {
    fn NSHomeDirectory() -> *mut c_void;
}
#[link(name = "objc")]
extern "C" {
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}
extern "C" {
    fn SDL_UIKitRunApp(
        argc: i32,
        argv: *mut *mut c_char,
        callback: Option<unsafe extern "C" fn(i32, *mut *mut c_char) -> i32>,
    ) -> i32;
}

fn documents() -> Result<PathBuf, String> {
    unsafe {
        let string = NSHomeDirectory();
        if string.is_null() {
            return Err("NSHomeDirectory returned nil".into());
        }
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *const c_char =
            std::mem::transmute(objc_msgSend as *const ());
        let value = f(string, sel_registerName(b"UTF8String\0".as_ptr().cast()));
        if value.is_null() {
            return Err("Home directory UTF8String returned nil".into());
        }
        Ok(PathBuf::from(CStr::from_ptr(value).to_string_lossy().as_ref()).join("Documents"))
    }
}

fn initialize() -> Result<PathBuf, String> {
    let docs = documents()?;
    std::fs::create_dir_all(&docs).map_err(|e| e.to_string())?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(docs.join("host-log.txt"))
        .map_err(|e| e.to_string())?;
    unsafe {
        if libc::dup2(log.as_raw_fd(), libc::STDOUT_FILENO) < 0
            || libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO) < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    std::env::set_var("TOUCHHLE_USER_DATA_DIR", &docs);
    std::env::set_var("TOUCHHLE_CPU_BACKEND", "interpreter");
    std::env::set_var("RUST_BACKTRACE", "1");
    let bundle = std::env::current_exe().map_err(|e| e.to_string())?;
    let base = bundle.parent().ok_or("Executable has no parent")?;
    let library = docs
        .parent()
        .ok_or("Documents has no parent")?
        .join("Library");
    let resources = crate::host_resource_cache::prepare(base, &library)?;
    std::env::set_var("TOUCHHLE_IOS_RESOURCES_DIR", &resources);
    std::env::set_current_dir(&resources).map_err(|e| e.to_string())?;
    eprintln!("\n[IOSHost] Android-based HLE host 0.4.2 / interpreter / no JIT");
    eprintln!(
        "[IOSHost] bundle={} documents={}",
        base.display(),
        docs.display()
    );
    eprintln!("[IOSHost] HLE resource data={}", resources.display());
    eprintln!("[IOSHost] native page bytes={}", unsafe {
        libc::sysconf(libc::_SC_PAGESIZE)
    });
    match std::fs::OpenOptions::new().write(true).create_new(true).open(docs.join("START-HERE.txt")) {
        Ok(mut file) => file.write_all(b"Put the unchanged DeadSpace.ipa in this Documents directory, then reopen the host.\nThe host uses the ARMv7 interpreter. host-log.txt contains the startup and runtime log.\nNo game is bundled. Existing game files and saves are never replaced by setup.\n").map_err(|e| e.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.to_string()),
    }
    Ok(docs)
}

unsafe extern "C" fn run_core(_: i32, _: *mut *mut c_char) -> i32 {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let docs = documents()?;
        // SDLUIKitDelegate resets cwd after initialize(), before this callback.
        let resources = std::env::var_os("TOUCHHLE_IOS_RESOURCES_DIR")
            .ok_or("iOS host resource directory is missing after SDL launch")?;
        std::env::set_current_dir(&resources).map_err(|e| e.to_string())?;
        eprintln!("[IOSHost] resource directory restored after SDL UIKit bootstrap");
        let game = docs.join("DeadSpace.ipa");
        let mut args = vec!["DSNoJITHost".to_owned()];
        if game.is_file() {
            eprintln!("[IOSHost] attempting original guest {}", game.display());
            args.push(game.to_string_lossy().into_owned());
        } else {
            eprintln!("[IOSHost] original guest absent; showing mobile setup screen");
            return touchHLE::show_ios_landing("Dead Space No-JIT Host 0.4.2\n\nAndroid mobile baseline\n\nCopy the unchanged DeadSpace.ipa to this app\'s Documents folder, then reopen.\n\nTouch controls only. No game is bundled.");
        }
        args.push("--gles1=gles1_native".to_owned());
        let result = touchHLE::main(args.into_iter());
        result
    }));
    let code = match outcome {
        Ok(Ok(())) => {
            eprintln!("[IOSHost] core returned normally");
            0
        }
        Ok(Err(error)) => {
            eprintln!("[IOSHost] core failed: {error}");
            1
        }
        Err(_) => {
            eprintln!("[IOSHost] panic caught before the UIKit C boundary");
            2
        }
    };
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    code
}

pub fn main() {
    if let Err(error) = initialize() {
        eprintln!("[IOSHost] setup failed: {error}");
        return;
    }
    // SDL 2.26 owns the UIApplication delegate. Do not create a second UIKit loop.
    let mut name = b"DSNoJITHost\0".to_vec();
    let mut argv = [name.as_mut_ptr().cast::<c_char>(), std::ptr::null_mut()];
    unsafe {
        SDL_UIKitRunApp(1, argv.as_mut_ptr(), Some(run_core));
    }
}
