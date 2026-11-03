/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
//! A resource path must survive SDL UIKit changing the process directory.
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

pub(crate) fn resolve(base: Option<&OsStr>, relative: &str) -> Result<PathBuf, String> {
    let base = PathBuf::from(base.ok_or("iOS host resource directory is not configured")?);
    if !base.is_absolute() {
        return Err("iOS host resource directory must be absolute".into());
    }
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err("Host resource path escapes the resource directory".into());
    }
    Ok(base.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static DIRECTORY_LOCK: Mutex<()> = Mutex::new(());
    struct RestoreDirectory(PathBuf);
    impl Drop for RestoreDirectory {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    #[test]
    fn resource_read_survives_the_uikit_launch_directory_change() {
        let _lock = DIRECTORY_LOCK.lock().unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("ds-ios-location-{}-{stamp}", std::process::id()));
        let resources = root.join("private resource cache");
        let bundle = root.join("NativeHost.app");
        std::fs::create_dir_all(resources.join("touchHLE_fonts")).unwrap();
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(
            resources.join("touchHLE_fonts/LiberationSans-Regular.ttf"),
            b"font marker",
        )
        .unwrap();
        let _restore = RestoreDirectory(std::env::current_dir().unwrap());
        // SDLUIKitDelegate does exactly this after the host initializes.
        std::env::set_current_dir(&bundle).unwrap();
        let file = resolve(
            Some(resources.as_os_str()),
            "touchHLE_fonts/LiberationSans-Regular.ttf",
        )
        .unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"font marker");
        assert!(!Path::new("touchHLE_fonts/LiberationSans-Regular.ttf").exists());
    }
    #[test]
    fn relative_base_is_rejected() {
        assert!(resolve(Some(OsStr::new("relative")), "touchHLE_fonts/font.ttf").is_err());
    }
    #[test]
    fn parent_escape_is_rejected() {
        let root = std::env::temp_dir();
        assert!(resolve(Some(root.as_os_str()), "../Documents/DeadSpace.ipa").is_err());
    }
}
