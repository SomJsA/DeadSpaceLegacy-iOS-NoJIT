/* This Source Code Form is subject to the Mozilla Public License, v. 2.0. */
//! Unpack HLE support libraries as data, outside the native app's code bundle.
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

pub fn prepare(bundle: &Path, library: &Path) -> Result<PathBuf, String> {
    let input = bundle.join("host-resources.zip");
    let mut file = File::open(&input).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    let hash = format!("{:x}", digest.finalize());
    let root = library.join("Application Support/DSNoJITHost");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let cache = root.join(&hash);
    if cache.exists() {
        if fs::read_to_string(cache.join(".complete")).ok().as_deref() == Some(&hash) {
            return Ok(cache);
        }
        return Err("Resource cache is incomplete; existing files were preserved".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let stage = root.join(format!("extract-{}-{stamp}-{sequence}", std::process::id()));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    extract(&input, &stage)?;
    fs::write(stage.join(".complete"), hash).map_err(|e| e.to_string())?;
    fs::rename(&stage, &cache).map_err(|e| e.to_string())?;
    Ok(cache)
}

fn extract(input: &Path, output: &Path) -> Result<(), String> {
    let file = File::open(input).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    if archive.len() > 128 {
        return Err("Too many resource archive entries".into());
    }
    let mut names = HashSet::new();
    let mut bytes = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            return Err("Resource archive must contain regular files only".into());
        }
        if entry.name().contains('\\') {
            return Err("Invalid resource path separator".into());
        }
        let path = entry
            .enclosed_name()
            .ok_or("Resource archive path escapes its root")?
            .to_owned();
        let parts: Vec<_> = path.components().collect();
        let valid = parts.len() >= 2
            && (path.starts_with("touchHLE_dylibs") || path.starts_with("touchHLE_fonts") || path.starts_with("touchHLE_ds_fonts"))
            || parts.len() == 1 && path == Path::new("touchHLE_default_options.txt");
        if !valid || !names.insert(path.clone()) {
            return Err("Unexpected or duplicate resource path".into());
        }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 != 0 && mode & 0o170000 != 0o100000 {
                return Err("Resource archive contains a nonregular file".into());
            }
        }
        bytes = bytes
            .checked_add(entry.size())
            .ok_or("Resource size overflow")?;
        if bytes > 64 * 1024 * 1024 {
            return Err("Resource archive exceeds 64 MiB".into());
        }
        let target = output.join(&path);
        fs::create_dir_all(target.parent().ok_or("No resource parent")?)
            .map_err(|e| e.to_string())?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|e| e.to_string())?;
        let expected = entry.size();
        let copied = std::io::copy(&mut (&mut entry).take(expected + 1), &mut destination)
            .map_err(|e| e.to_string())?;
        if copied != expected {
            return Err("Resource byte count differs from archive".into());
        }
        destination.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(entries: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "ds-ios-res-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&base).unwrap();
        let input = base.join("host-resources.zip");
        let mut archive = zip::ZipWriter::new(File::create(&input).unwrap());
        for (name, bytes) in entries {
            archive
                .start_file(*name, zip::write::FileOptions::default())
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
        (base, input)
    }
    #[test]
    fn support_data_extracts_and_existing_complete_cache_is_reused() {
        let (base, _) = fixture(&[("touchHLE_dylibs/test.dylib", b"guest data")]);
        let cache = prepare(&base, &base.join("Library")).unwrap();
        assert_eq!(
            fs::read(cache.join("touchHLE_dylibs/test.dylib")).unwrap(),
            b"guest data"
        );
        assert_eq!(prepare(&base, &base.join("Library")).unwrap(), cache);
    }
    #[test]
    fn traversal_archive_never_writes_outside_extraction_root() {
        let (base, input) = fixture(&[("../escape.bin", b"bad")]);
        let target = base.join("new");
        fs::create_dir(&target).unwrap();
        assert!(extract(&input, &target).is_err());
        assert!(!base.join("escape.bin").exists());
    }
    #[test]
    fn duplicate_archive_entry_is_rejected_without_replacing_first_file() {
        let (base, input) = fixture(&[
            ("touchHLE_fonts/test.ttf", b"first"),
            ("touchHLE_fonts/test.ttf", b"second"),
        ]);
        let target = base.join("new");
        fs::create_dir(&target).unwrap();
        assert!(extract(&input, &target).is_err());
        assert_eq!(
            fs::read(target.join("touchHLE_fonts/test.ttf")).unwrap(),
            b"first"
        );
    }
    #[test]
    fn extraction_does_not_replace_existing_file() {
        let (base, input) = fixture(&[("touchHLE_fonts/test.ttf", b"new")]);
        let target = base.join("new");
        fs::create_dir_all(target.join("touchHLE_fonts")).unwrap();
        fs::write(target.join("touchHLE_fonts/test.ttf"), b"keep").unwrap();
        assert!(extract(&input, &target).is_err());
        assert_eq!(
            fs::read(target.join("touchHLE_fonts/test.ttf")).unwrap(),
            b"keep"
        );
    }
}

#[cfg(test)]
#[path = "ios_resource_location.rs"]
mod ios_resource_location;
