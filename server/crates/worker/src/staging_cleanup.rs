use anyhow::{Context, Result};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

const SOURCE_MARKER: &str = ".launcher-source-path";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub package_removed: bool,
    pub source_removed: bool,
    pub staging_objects_removed: usize,
}

pub fn enabled() -> bool {
    env::var("LAUNCHER_CLEANUP_STAGING_AFTER_PUBLISH")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

pub fn record_source(output: &Path, input: &Path, storage_root: &Path) -> Result<()> {
    let root = canonical_root(storage_root)?;
    let package = validate_staging_path(output, &root, "package output")?;
    let source = validate_staging_path(input, &root, "ingest input")?;
    if source == package || source.starts_with(&package) {
        anyhow::bail!("ingest input must not be inside the package output directory");
    }
    let marker = package.join(SOURCE_MARKER);
    write_marker(&marker, &source)
}

pub fn cleanup_after_publish(
    package: &Path,
    storage_root: &Path,
    staging_objects: &[(String, String)],
) -> Result<CleanupReport> {
    let root = canonical_root(storage_root)?;
    let package = validate_staging_path(package, &root, "published package")?;
    if package == root {
        anyhow::bail!("refusing to remove the storage root as a published package");
    }
    let source = read_source_marker(&package, &root)?;
    if let Some(source) = source.as_ref()
        && (source == &package || source.starts_with(&package))
    {
        anyhow::bail!("staging source must not be inside the published package");
    }

    let mut report = CleanupReport::default();
    for (object_key, encoded_hash) in staging_objects {
        let expected_key = format!("chunks/encoded/{encoded_hash}.bin");
        if object_key != &expected_key {
            anyhow::bail!("unexpected staging object key for {encoded_hash}: {object_key}");
        }
        if !is_lower_hex_hash(encoded_hash) {
            anyhow::bail!("invalid staging object hash: {encoded_hash}");
        }
        let path = root.join(&expected_key);
        if !path.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("could not inspect staging object {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!(
                "refusing to remove non-regular staging object {}",
                path.display()
            );
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("could not read staging object {}", path.display()))?;
        if blake3::hash(&bytes).to_hex().as_str() != encoded_hash {
            anyhow::bail!("staging object hash mismatch: {encoded_hash}");
        }
        fs::remove_file(&path)
            .with_context(|| format!("could not remove staging object {}", path.display()))?;
        report.staging_objects_removed += 1;
    }

    fs::remove_dir_all(&package)
        .with_context(|| format!("could not remove published package {}", package.display()))?;
    report.package_removed = true;

    if let Some(source) = source {
        report.source_removed = remove_staging_path(&source, &root)?;
        if report.source_removed {
            if let Some(parent) = source.parent() {
                remove_generated_handoff(parent, &source)?;
                remove_empty_parent_dirs(parent, &root)?;
            }
        }
    }
    Ok(report)
}

fn canonical_root(path: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(path)
        .with_context(|| format!("could not resolve staging storage root {}", path.display()))?;
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!(
            "staging storage root is not a real directory: {}",
            root.display()
        );
    }
    Ok(root)
}

fn validate_staging_path(path: &Path, root: &Path, label: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("refusing symlink {label}: {}", path.display());
    }
    let resolved = fs::canonicalize(path)
        .with_context(|| format!("could not resolve {label} {}", path.display()))?;
    if !resolved.starts_with(root) {
        anyhow::bail!(
            "{label} is outside the staging storage root: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn read_source_marker(package: &Path, root: &Path) -> Result<Option<PathBuf>> {
    let marker = package.join(SOURCE_MARKER);
    if !marker.exists() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(&marker)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!(
            "refusing invalid staging cleanup marker: {}",
            marker.display()
        );
    }
    let value = fs::read_to_string(&marker)
        .with_context(|| format!("could not read staging cleanup marker {}", marker.display()))?;
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("staging cleanup marker is empty: {}", marker.display());
    }
    Ok(Some(validate_staging_path(
        Path::new(value),
        root,
        "ingest input",
    )?))
}

fn write_marker(marker: &Path, source: &Path) -> Result<()> {
    let temporary = marker.with_extension("tmp");
    fs::write(&temporary, format!("{}\n", source.display())).with_context(|| {
        format!(
            "could not write staging cleanup marker {}",
            marker.display()
        )
    })?;
    if let Err(error) = fs::rename(&temporary, marker) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| {
            format!(
                "could not commit staging cleanup marker {}",
                marker.display()
            )
        });
    }
    Ok(())
}

fn remove_staging_path(path: &Path, root: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        anyhow::bail!("refusing symlink staging input: {}", path.display());
    }
    let resolved = fs::canonicalize(path)?;
    if resolved == root || !resolved.starts_with(root) {
        anyhow::bail!(
            "staging input is outside the storage root: {}",
            resolved.display()
        );
    }
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn remove_generated_handoff(parent: &Path, source: &Path) -> Result<()> {
    let handoff = parent.join("handoff.json");
    if handoff == source {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(&handoff) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("refusing invalid generated handoff: {}", handoff.display());
    }
    fs::remove_file(handoff)?;
    Ok(())
}

fn remove_empty_parent_dirs(start: &Path, root: &Path) -> Result<()> {
    let mut current = start.to_owned();
    loop {
        let resolved = fs::canonicalize(&current)?;
        if resolved == root {
            break;
        }
        if !resolved.starts_with(root) {
            anyhow::bail!(
                "staging directory is outside the storage root: {}",
                resolved.display()
            );
        }
        match fs::remove_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) => return Err(error.into()),
        }
        current = match current.parent() {
            Some(parent) => parent.to_owned(),
            None => break,
        };
    }
    Ok(())
}

fn is_lower_hex_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_root(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "launcher-staging-cleanup-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn cleanup_removes_package_source_and_verified_staging_objects() {
        let root = test_root("success");
        let source = root.join("scraper/artifacts/release/game.zip");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"source").unwrap();
        fs::write(source.parent().unwrap().join("handoff.json"), b"{}\n").unwrap();
        let package = root.join("scraper/packages/release");
        fs::create_dir_all(package.join("chunks/encoded")).unwrap();
        let bytes = b"encoded chunk";
        let hash = blake3::hash(bytes).to_hex().to_string();
        let object_key = format!("chunks/encoded/{hash}.bin");
        fs::write(package.join(&object_key), bytes).unwrap();
        let staging_object = root.join(&object_key);
        fs::create_dir_all(staging_object.parent().unwrap()).unwrap();
        fs::write(&staging_object, bytes).unwrap();
        write_marker(&package.join(SOURCE_MARKER), &source).unwrap();

        let report = cleanup_after_publish(&package, &root, &[(object_key, hash)]).unwrap();

        assert_eq!(
            report,
            CleanupReport {
                package_removed: true,
                source_removed: true,
                staging_objects_removed: 1,
            }
        );
        assert!(!package.exists());
        assert!(!source.exists());
        assert!(!source.parent().unwrap().exists());
        assert!(!staging_object.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_refuses_paths_outside_the_storage_root() {
        let root = test_root("refuse");
        let outside = test_root("outside");
        let package = root.join("package");
        fs::create_dir_all(&package).unwrap();
        let marker = package.join(SOURCE_MARKER);
        let mut file = fs::File::create(&marker).unwrap();
        writeln!(file, "{}", outside.display()).unwrap();

        let error = cleanup_after_publish(&package, &root, &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside the staging storage root")
        );
        assert!(package.exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
