//! `terra admin export` / `import` — the whole data directory as one zip.
//!
//! Everything terrarium knows lives in plain files under `TERRARIUM_DATA`, so
//! a backup is those files. Each file is written atomically by the server,
//! so exporting while it runs yields a consistent copy of every file; a push
//! landing mid-export may or may not be included. Import is meant for an
//! empty data directory with the server stopped.
//!
//! The archive holds password hashes, session tokens' hashes and webhook
//! signing secrets: it is created `0600`, and file modes are preserved.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MANIFEST: &str = "terrarium-export.json";

/// Top-level entries that mark a directory as already holding terrarium data.
const DATA_MARKERS: &[&str] = &["state", "versions", "locks", "users", "webhooks.json"];

#[derive(Debug)]
pub struct Summary {
    pub files: usize,
    pub bytes: u64,
}

/// Write every file under `data` into a zip at `out`. Provider binaries in
/// `registry/` are large and re-downloadable, so they're only included with
/// `include_registry`.
pub fn export(data: &Path, out: &Path, include_registry: bool) -> Result<Summary, String> {
    let file = create_private(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let out_abs = fs::canonicalize(out).ok();

    let mut files = Vec::new();
    collect(data, data, &mut files).map_err(|e| format!("cannot read {}: {e}", data.display()))?;
    files.sort();

    let mut summary = Summary { files: 0, bytes: 0 };
    for rel in files {
        let path = data.join(&rel);
        let name = rel.to_string_lossy().replace('\\', "/");
        let skip = name.ends_with(".tmp")
            || (!include_registry && name.starts_with("registry/"))
            || out_abs.as_deref().is_some_and(|o| fs::canonicalize(&path).ok().as_deref() == Some(o));
        if skip {
            continue;
        }
        let mut contents = Vec::new();
        match fs::File::open(&path).and_then(|mut f| f.read_to_end(&mut contents)) {
            Ok(_) => {}
            // Deleted between listing and reading (a pruned version, a
            // released lock): not part of this snapshot.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        }
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode(&path));
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        zip.write_all(&contents).map_err(|e| e.to_string())?;
        summary.files += 1;
        summary.bytes += contents.len() as u64;
    }

    let manifest = serde_json::json!({
        "terrarium": env!("CARGO_PKG_VERSION"),
        "created": chrono::Utc::now().to_rfc3339(),
        "registry": include_registry,
    });
    zip.start_file(MANIFEST, zip::write::SimpleFileOptions::default()).map_err(|e| e.to_string())?;
    zip.write_all(manifest.to_string().as_bytes()).map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(summary)
}

/// Unpack an export into `data`. Refuses a directory that already holds
/// terrarium data unless `force`, in which case archive files overwrite
/// existing ones and nothing else is removed.
pub fn import(data: &Path, archive: &Path, force: bool) -> Result<Summary, String> {
    let file = fs::File::open(archive).map_err(|e| format!("cannot open {}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{} is not a zip archive: {e}", archive.display()))?;
    if zip.by_name(MANIFEST).is_err() {
        return Err(format!("{} is not a terrarium export (no {MANIFEST})", archive.display()));
    }
    if !force {
        if let Some(existing) = DATA_MARKERS.iter().find(|m| data.join(m).exists()) {
            return Err(format!(
                "{} already holds terrarium data ({existing}); import into an empty directory, or pass --force to overwrite",
                data.display()
            ));
        }
    }

    let mut summary = Summary { files: 0, bytes: 0 };
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() || entry.name() == MANIFEST {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..`, so an archive
        // can't write outside the data directory.
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("refusing unsafe path in archive: {:?}", entry.name()));
        };
        let target = data.join(&rel);
        let mut contents = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut contents).map_err(|e| e.to_string())?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        fs::write(&target, &contents).map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777));
        }
        summary.files += 1;
        summary.bytes += contents.len() as u64;
    }
    Ok(summary)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &path, out)?;
        } else if kind.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}

fn create_private(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(meta) = fs::metadata(path) {
            return meta.permissions().mode() & 0o777;
        }
    }
    let _ = path;
    0o644
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("terrarium-backup-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn round_trip_preserves_files_and_modes() {
        let src = tmp("src");
        fs::create_dir_all(src.join("versions/infra/prod")).unwrap();
        fs::create_dir_all(src.join("registry/providers")).unwrap();
        fs::write(src.join("versions/infra/prod/1"), b"state").unwrap();
        fs::write(src.join("registry/providers/big.zip"), b"binary").unwrap();
        fs::write(src.join("versions/infra/prod/.1.123.0.tmp"), b"junk").unwrap();
        crate::state::atomic_write_private(&src.join("webhooks.json"), b"[]").unwrap();

        let out = tmp("out").join("backup.zip");
        let s = export(&src, &out, false).unwrap();
        assert_eq!(s.files, 2, "registry and temp files are left out");

        let dst = tmp("dst");
        import(&dst, &out, false).unwrap();
        assert_eq!(fs::read(dst.join("versions/infra/prod/1")).unwrap(), b"state");
        assert!(!dst.join("registry").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(dst.join("webhooks.json")).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
        }

        // A second import into the now-populated directory needs --force.
        assert!(import(&dst, &out, false).unwrap_err().contains("already holds"));
        assert!(import(&dst, &out, true).is_ok());

        for d in [src, dst, out.parent().unwrap().to_path_buf()] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn import_rejects_foreign_archives() {
        let dir = tmp("foreign");
        let path = dir.join("x.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("hello.txt", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"hi").unwrap();
        zip.finish().unwrap();
        assert!(import(&dir.join("data"), &path, false).unwrap_err().contains("not a terrarium export"));
        let _ = fs::remove_dir_all(dir);
    }
}
