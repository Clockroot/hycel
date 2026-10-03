//! Bounded native bundle creation and deterministic USTAR packaging.
//!
//! Filesystem validation is point-in-time and does not prevent concurrent tree replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    CliOutput, MAX_DIRECTORY_DEPTH, MAX_ENTRIES_PER_DIRECTORY, MAX_PROJECT_DIRECTORIES,
    MAX_PROJECT_ENTRIES, cli_diagnostic, failure, load_project, success,
};

const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
const BUILD_MANIFEST: &str = "BUILD.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    schema_version: u32,
    engine_version: String,
    target_triple: String,
    project_id: String,
    project_name: String,
    binary_name: String,
    binary_bytes: u64,
    included_project_bytes: u64,
}

#[derive(Serialize)]
struct BuildResult {
    output_path: String,
    target_triple: String,
    project_id: String,
    included_project_bytes: u64,
    executable_name: String,
}

#[derive(Serialize)]
struct PackageResult {
    archive_path: String,
    target_triple: String,
    bytes_written: u64,
    format: &'static str,
}

#[derive(Clone, Debug)]
struct BundleEntry {
    path: PathBuf,
    relative_path: String,
    directory: bool,
    bytes: u64,
    mode: u32,
}

#[derive(Default)]
struct CopyBudget {
    entries: usize,
    directories: usize,
    bytes: u64,
}

/// Create an experimental, current-host bundle for the compiled-in reference game.
pub(super) fn build_project(project_path: &Path, output_path: &Path, json: bool) -> CliOutput {
    let loaded = match load_project(project_path) {
        Ok(loaded) => loaded,
        Err(diagnostics) => return failure("build", 1, diagnostics, json),
    };
    let scenarios = match hycel_demo::test_playable_platformer(project_path, None) {
        Ok(scenarios) => scenarios,
        Err(error) => {
            return failure(
                "build",
                1,
                vec![cli_diagnostic(
                    "HYCEL-CLI-150",
                    project_path.to_str(),
                    "reference_game",
                    format!(
                        "project is not compatible with the compiled-in reference game: {error}"
                    ),
                )],
                json,
            );
        }
    };
    if let Some(failed) = scenarios.iter().find(|scenario| !scenario.passed) {
        return failure(
            "build",
            1,
            vec![cli_diagnostic(
                "HYCEL-CLI-151",
                project_path.to_str(),
                &failed.name,
                failed.message.clone(),
            )],
            json,
        );
    }
    let Some(target_triple) = native_target_triple() else {
        return failure(
            "build",
            3,
            vec![cli_diagnostic(
                "HYCEL-CLI-152",
                None,
                "target",
                "native bundles are unavailable for this OS/architecture pair",
            )],
            json,
        );
    };
    let project_root = match project_path.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return build_io_failure(
                output_path,
                &format!("cannot resolve project root: {error}"),
                json,
            );
        }
    };
    let destination = match reserve_output_directory(output_path, &project_root) {
        Ok(path) => path,
        Err(error) => return build_io_failure(output_path, &error, json),
    };
    let build_result = build_into(&project_root, &destination, &loaded, target_triple);
    let (manifest, executable_name) = match build_result {
        Ok(result) => result,
        Err(error) => {
            return failure(
                "build",
                3,
                vec![cli_diagnostic(
                    "HYCEL-CLI-153",
                    destination.to_str(),
                    "bundle",
                    format!(
                        "bundle creation failed; partial output was preserved for inspection: {error}"
                    ),
                )],
                json,
            );
        }
    };
    let result = BuildResult {
        output_path: destination.to_string_lossy().into_owned(),
        target_triple: manifest.target_triple,
        project_id: manifest.project_id,
        included_project_bytes: manifest.included_project_bytes,
        executable_name,
    };
    let human = format!(
        "Built experimental {} bundle at {}\n",
        result.target_triple, result.output_path
    );
    success("build", result, json, &human)
}

fn build_into(
    project_root: &Path,
    destination: &Path,
    loaded: &super::LoadedProject,
    target_triple: &str,
) -> Result<(BundleManifest, String), String> {
    let game_directory = destination.join("game");
    fs::create_dir(&game_directory)
        .map_err(|error| format!("cannot create game data directory: {error}"))?;
    let mut budget = CopyBudget::default();
    for file in ["hycel.toml", "input.json"] {
        let source = project_root.join(file);
        match fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                copy_project_file(
                    project_root,
                    &source,
                    &game_directory.join(file),
                    &mut budget,
                )?;
            }
            Ok(_) => return Err(format!("project root entry {file} is not a regular file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound && file == "input.json" => {}
            Err(error) => return Err(format!("cannot inspect project file {file}: {error}")),
        }
    }
    for directory in ["assets", "scenes", "src"] {
        let source = project_root.join(directory);
        let metadata = fs::symlink_metadata(&source)
            .map_err(|error| format!("cannot inspect project directory {directory}: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "project directory {directory} is not a real directory"
            ));
        }
        copy_project_tree(
            project_root,
            &source,
            &game_directory.join(directory),
            directory,
            0,
            &mut budget,
        )?;
    }
    let current_exe = std::env::current_exe()
        .map_err(|error| format!("cannot identify the running Hycel executable: {error}"))?;
    let executable_name = format!("hycel{}", std::env::consts::EXE_SUFFIX);
    let executable_path = destination.join(&executable_name);
    let executable_bytes = fs::metadata(&current_exe)
        .map_err(|error| format!("cannot inspect the running executable: {error}"))?
        .len();
    if executable_bytes > MAX_BUNDLE_BYTES.saturating_sub(64 * 1024)
        || budget.bytes.saturating_add(executable_bytes)
            > MAX_BUNDLE_BYTES.saturating_sub(64 * 1024)
    {
        return Err(format!(
            "executable and project data exceed the {MAX_BUNDLE_BYTES}-byte bundle budget"
        ));
    }
    fs::copy(&current_exe, &executable_path)
        .map_err(|error| format!("cannot copy the running executable: {error}"))?;
    write_launcher(destination, &executable_name)?;
    let manifest = BundleManifest {
        schema_version: 1,
        engine_version: env!("CARGO_PKG_VERSION").to_owned(),
        target_triple: target_triple.to_owned(),
        project_id: loaded.manifest.project_id().to_owned(),
        project_name: loaded.manifest.project_name().to_owned(),
        binary_name: executable_name.clone(),
        binary_bytes: executable_bytes,
        included_project_bytes: budget.bytes,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot encode bundle manifest: {error}"))?;
    write_new_file(&destination.join(BUILD_MANIFEST), &manifest_bytes)
        .map_err(|error| format!("cannot write build manifest: {error}"))?;
    Ok((manifest, executable_name))
}

fn copy_project_tree(
    root: &Path,
    source_directory: &Path,
    output_directory: &Path,
    relative_directory: &str,
    depth: usize,
    budget: &mut CopyBudget,
) -> Result<(), String> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(format!(
            "project directory tree exceeds the {MAX_DIRECTORY_DEPTH}-level depth limit"
        ));
    }
    fs::create_dir(output_directory).map_err(|error| {
        format!(
            "cannot create bundled directory {}: {error}",
            output_directory.display()
        )
    })?;
    budget.directories = budget.directories.saturating_add(1);
    if budget.directories > MAX_PROJECT_DIRECTORIES {
        return Err(format!(
            "project directory count exceeds {MAX_PROJECT_DIRECTORIES}"
        ));
    }
    let mut entries = fs::read_dir(source_directory)
        .map_err(|error| {
            format!(
                "cannot read project directory {}: {error}",
                source_directory.display()
            )
        })?
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if entries.len() > MAX_ENTRIES_PER_DIRECTORY {
        return Err(format!(
            "directory {} exceeds the {}-entry limit",
            source_directory.display(),
            MAX_ENTRIES_PER_DIRECTORY
        ));
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        budget.entries = budget.entries.saturating_add(1);
        if budget.entries > MAX_PROJECT_ENTRIES {
            return Err(format!("project entry count exceeds {MAX_PROJECT_ENTRIES}"));
        }
        let source = entry.path();
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else {
            return Err(format!(
                "project entry under {relative_directory} is not UTF-8"
            ));
        };
        let relative = format!("{relative_directory}/{name_text}");
        let metadata = fs::symlink_metadata(&source)
            .map_err(|error| format!("cannot inspect project entry {relative}: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("project entry {relative} is a symlink"));
        }
        let destination = output_directory.join(&name);
        if metadata.is_dir() {
            copy_project_tree(root, &source, &destination, &relative, depth + 1, budget)?;
        } else if metadata.is_file() {
            copy_project_file(root, &source, &destination, budget)?;
        } else {
            return Err(format!(
                "project entry {relative} is not a regular file or directory"
            ));
        }
    }
    Ok(())
}

fn copy_project_file(
    root: &Path,
    source: &Path,
    destination: &Path,
    budget: &mut CopyBudget,
) -> Result<(), String> {
    let canonical = source
        .canonicalize()
        .map_err(|error| format!("cannot resolve project file {}: {error}", source.display()))?;
    if !canonical.starts_with(root) || canonical != source {
        return Err(format!(
            "project file {} is not a canonical in-root file",
            source.display()
        ));
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("cannot inspect project file {}: {error}", source.display()))?;
    let length = metadata.len();
    if length > MAX_BUNDLE_BYTES || budget.bytes.saturating_add(length) > MAX_BUNDLE_BYTES {
        return Err("project content exceeds the bundle file/aggregate byte limit".to_owned());
    }
    let mut input = File::open(&canonical)
        .map_err(|error| format!("cannot open project file {}: {error}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            format!(
                "cannot create bundled file {}: {error}",
                destination.display()
            )
        })?;
    let copied = io::copy(&mut input, &mut output)
        .map_err(|error| format!("cannot copy project file {}: {error}", source.display()))?;
    if copied != length {
        return Err(format!(
            "project file {} changed while it was copied",
            source.display()
        ));
    }
    output.sync_all().map_err(|error| {
        format!(
            "cannot sync bundled file {}: {error}",
            destination.display()
        )
    })?;
    budget.bytes = budget.bytes.saturating_add(copied);
    Ok(())
}

fn write_launcher(destination: &Path, executable_name: &str) -> Result<(), String> {
    if cfg!(windows) {
        let launcher = format!("@echo off\r\n\"%~dp0{executable_name}\" run \"%~dp0game\" %*\r\n");
        write_new_file(&destination.join("run.cmd"), launcher.as_bytes())
            .map_err(|error| format!("cannot write Windows launcher: {error}"))
    } else {
        let launcher = format!(
            "#!/bin/sh\nset -eu\nDIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\nexec \"$DIR/{executable_name}\" run \"$DIR/game\" \"$@\"\n"
        );
        let path = destination.join("run.sh");
        write_new_file(&path, launcher.as_bytes())
            .map_err(|error| format!("cannot write native launcher: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("cannot set launcher permissions: {error}"))?;
        }
        Ok(())
    }
}

fn reserve_output_directory(output: &Path, project_root: &Path) -> Result<PathBuf, String> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("output parent must already exist: {error}"))?;
    let file_name = output
        .file_name()
        .ok_or_else(|| "output path must name a new directory".to_owned())?;
    let destination = parent.join(file_name);
    if destination.starts_with(project_root) {
        return Err("build output must be outside the source project".to_owned());
    }
    fs::create_dir(&destination)
        .map_err(|error| format!("cannot reserve new output directory: {error}"))?;
    Ok(destination)
}

fn build_io_failure(output_path: &Path, message: &str, json: bool) -> CliOutput {
    failure(
        "build",
        3,
        vec![cli_diagnostic(
            "HYCEL-CLI-154",
            output_path.to_str(),
            "output",
            message,
        )],
        json,
    )
}

fn supported_target_triple(target: &str) -> bool {
    [
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ]
    .contains(&target)
}

fn native_target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

/// Package a completed native bundle in deterministic POSIX USTAR format.
#[allow(clippy::too_many_lines)] // Keep package validation and cleanup flow visible in one audit path.
pub(super) fn package_bundle(bundle_path: &Path, archive_path: &Path, json: bool) -> CliOutput {
    let metadata = match fs::symlink_metadata(bundle_path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => {
            return package_failure(archive_path, "bundle path must be a real directory", json);
        }
        Err(error) => {
            return package_failure(
                archive_path,
                &format!("cannot inspect bundle: {error}"),
                json,
            );
        }
    };
    let _ = metadata;
    let root = match bundle_path.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            return package_failure(
                archive_path,
                &format!("cannot resolve bundle: {error}"),
                json,
            );
        }
    };
    let manifest_path = root.join(BUILD_MANIFEST);
    let manifest_metadata = match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => {
            return package_failure(
                archive_path,
                "BUILD.json must be a regular non-symlink file",
                json,
            );
        }
        Err(error) => {
            return package_failure(
                archive_path,
                &format!("cannot inspect BUILD.json: {error}"),
                json,
            );
        }
    };
    if manifest_metadata.len() > 64 * 1024
        || manifest_path.canonicalize().ok().as_deref() != Some(manifest_path.as_path())
    {
        return package_failure(
            archive_path,
            "BUILD.json is too large or resolves outside its bundle path",
            json,
        );
    }
    let manifest_bytes = match read_bounded(&manifest_path, 64 * 1024) {
        Ok(bytes) => bytes,
        Err(error) => return package_failure(archive_path, &error, json),
    };
    let manifest: BundleManifest = match serde_json::from_slice::<BundleManifest>(&manifest_bytes) {
        Ok(manifest) if manifest.schema_version == 1 => manifest,
        Ok(_) => {
            return package_failure(
                archive_path,
                "unsupported native bundle manifest version",
                json,
            );
        }
        Err(error) => {
            return package_failure(archive_path, &format!("invalid BUILD.json: {error}"), json);
        }
    };
    let expected_binary_name = if manifest.target_triple.contains("windows") {
        "hycel.exe"
    } else {
        "hycel"
    };
    if manifest.binary_name != expected_binary_name
        || !supported_target_triple(&manifest.target_triple)
    {
        return package_failure(
            archive_path,
            "BUILD.json contains an unsupported binary name or target triple",
            json,
        );
    }
    let executable = root.join(&manifest.binary_name);
    let executable_metadata = match fs::symlink_metadata(&executable) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => {
            return package_failure(
                archive_path,
                "bundle executable is not a regular file",
                json,
            );
        }
        Err(error) => {
            return package_failure(
                archive_path,
                &format!("cannot inspect bundle executable: {error}"),
                json,
            );
        }
    };
    if executable_metadata.len() != manifest.binary_bytes {
        return package_failure(
            archive_path,
            "bundle executable size does not match BUILD.json",
            json,
        );
    }
    let mut entries = Vec::new();
    let mut budget = CopyBudget::default();
    if let Err(error) = collect_bundle_entries(&root, &root, "", 0, &mut entries, &mut budget) {
        return package_failure(archive_path, &error, json);
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    match estimate_archive_bytes(&entries) {
        Some(bytes) if bytes <= MAX_BUNDLE_BYTES => {}
        _ => {
            return package_failure(
                archive_path,
                "USTAR output exceeds the 512 MiB package limit",
                json,
            );
        }
    }
    let destination = match reserve_archive_path(archive_path, &root) {
        Ok(destination) => destination,
        Err(error) => return package_failure(archive_path, &error, json),
    };
    let mut output = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(output) => output,
        Err(error) => {
            return package_failure(
                archive_path,
                &format!("cannot create new archive: {error}"),
                json,
            );
        }
    };
    let archive_result = (|| -> io::Result<()> {
        for entry in &entries {
            write_tar_entry(&mut output, entry)?;
        }
        output.write_all(&[0_u8; 1024])?;
        output.sync_all()
    })();
    if let Err(error) = archive_result {
        drop(output);
        let cleanup = fs::remove_file(&destination);
        let detail = cleanup.err().map_or_else(String::new, |cleanup| {
            format!("; incomplete archive cleanup failed: {cleanup}")
        });
        return package_failure(
            archive_path,
            &format!("cannot complete archive: {error}{detail}"),
            json,
        );
    }
    let bytes_written = match fs::metadata(&destination) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            return package_failure(
                archive_path,
                &format!("cannot inspect completed archive: {error}"),
                json,
            );
        }
    };
    let result = PackageResult {
        archive_path: destination.to_string_lossy().into_owned(),
        target_triple: manifest.target_triple,
        bytes_written,
        format: "application/x-tar; ustar",
    };
    let human = format!(
        "Wrote {} bytes of USTAR package to {}\n",
        result.bytes_written, result.archive_path
    );
    success("package", result, json, &human)
}

fn estimate_archive_bytes(entries: &[BundleEntry]) -> Option<u64> {
    entries.iter().try_fold(1024_u64, |total, entry| {
        let padded = entry
            .bytes
            .checked_add(511)?
            .checked_div(512)?
            .checked_mul(512)?;
        total
            .checked_add(512)?
            .checked_add(if entry.directory { 0 } else { padded })
    })
}

fn collect_bundle_entries(
    root: &Path,
    directory: &Path,
    relative: &str,
    depth: usize,
    entries: &mut Vec<BundleEntry>,
    budget: &mut CopyBudget,
) -> Result<(), String> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(format!(
            "bundle exceeds {MAX_DIRECTORY_DEPTH} directory levels"
        ));
    }
    let mut children = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "cannot read bundle directory {}: {error}",
                directory.display()
            )
        })?
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if children.len() > MAX_ENTRIES_PER_DIRECTORY {
        return Err(format!(
            "bundle directory exceeds {MAX_ENTRIES_PER_DIRECTORY} entries"
        ));
    }
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        budget.entries = budget.entries.saturating_add(1);
        if budget.entries > MAX_PROJECT_ENTRIES {
            return Err(format!("bundle exceeds {MAX_PROJECT_ENTRIES} entries"));
        }
        let path = child.path();
        let name = child.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| "bundle entry path is not UTF-8".to_owned())?;
        let relative_path = if relative.is_empty() {
            name.to_owned()
        } else {
            format!("{relative}/{name}")
        };
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect bundle entry {relative_path}: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("bundle entry {relative_path} is a symlink"));
        }
        let mode = file_mode(&metadata, metadata.is_dir());
        if metadata.is_dir() {
            budget.directories = budget.directories.saturating_add(1);
            if budget.directories > MAX_PROJECT_DIRECTORIES {
                return Err(format!(
                    "bundle exceeds {MAX_PROJECT_DIRECTORIES} directories"
                ));
            }
            entries.push(BundleEntry {
                path: path.clone(),
                relative_path: format!("{relative_path}/"),
                directory: true,
                bytes: 0,
                mode,
            });
            collect_bundle_entries(root, &path, &relative_path, depth + 1, entries, budget)?;
        } else if metadata.is_file() {
            if !path
                .canonicalize()
                .map_err(|error| error.to_string())?
                .starts_with(root)
            {
                return Err(format!("bundle file {relative_path} escapes its root"));
            }
            budget.bytes = budget.bytes.saturating_add(metadata.len());
            if budget.bytes > MAX_BUNDLE_BYTES {
                return Err(format!(
                    "bundle exceeds the {MAX_BUNDLE_BYTES}-byte package limit"
                ));
            }
            entries.push(BundleEntry {
                path,
                relative_path,
                directory: false,
                bytes: metadata.len(),
                mode,
            });
        } else {
            return Err(format!(
                "bundle entry {relative_path} is not a regular file or directory"
            ));
        }
    }
    Ok(())
}

fn file_mode(metadata: &fs::Metadata, directory: bool) -> u32 {
    #[cfg(unix)]
    let _ = directory;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        if directory { 0o755 } else { 0o644 }
    }
}

fn reserve_archive_path(output: &Path, bundle_root: &Path) -> Result<PathBuf, String> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("archive parent must already exist: {error}"))?;
    let name = output
        .file_name()
        .ok_or_else(|| "archive path must name a new file".to_owned())?;
    let destination = parent.join(name);
    if destination.starts_with(bundle_root) {
        return Err("archive output must be outside the bundle directory".to_owned());
    }
    Ok(destination)
}

fn write_tar_entry(output: &mut File, entry: &BundleEntry) -> io::Result<()> {
    let header = tar_header(
        &entry.relative_path,
        entry.bytes,
        entry.mode,
        entry.directory,
    )?;
    output.write_all(&header)?;
    if entry.directory {
        return Ok(());
    }
    let mut input = File::open(&entry.path)?;
    if input.metadata()?.len() != entry.bytes {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "bundle file changed after package inspection",
        ));
    }
    let copied = io::copy(&mut Read::by_ref(&mut input).take(entry.bytes), output)?;
    if copied != entry.bytes {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "bundle file shortened while packaging",
        ));
    }
    let padding = (512 - (entry.bytes % 512)) % 512;
    let zeroes = [0_u8; 512];
    output.write_all(&zeroes[..usize::try_from(padding).unwrap_or(0)])
}

fn tar_header(path: &str, size: u64, mode: u32, directory: bool) -> io::Result<[u8; 512]> {
    let (prefix, name) = split_ustar_path(path).map_err(invalid_tar_path)?;
    let mut header = [0_u8; 512];
    header[..name.len()].copy_from_slice(name);
    put_octal(&mut header[100..108], u64::from(mode))?;
    put_octal(&mut header[108..116], 0)?;
    put_octal(&mut header[116..124], 0)?;
    put_octal(&mut header[124..136], if directory { 0 } else { size })?;
    put_octal(&mut header[136..148], 0)?;
    header[148..156].fill(b' ');
    header[156] = if directory { b'5' } else { b'0' };
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[345..345 + prefix.len()].copy_from_slice(prefix);
    let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
    let checksum_text = format!("{checksum:06o}");
    if checksum_text.len() != 6 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "USTAR checksum overflow",
        ));
    }
    header[148..154].copy_from_slice(checksum_text.as_bytes());
    header[154] = 0;
    header[155] = b' ';
    Ok(header)
}

fn split_ustar_path(path: &str) -> Result<(&[u8], &[u8]), String> {
    let path = path.as_bytes();
    if path.len() <= 100 {
        return Ok((&[], path));
    }
    for (index, byte) in path.iter().enumerate().rev() {
        if *byte == b'/' && index <= 155 && path.len().saturating_sub(index + 1) <= 100 {
            return Ok((&path[..index], &path[index + 1..]));
        }
    }
    Err("bundle path cannot be represented by USTAR's 100/155-byte name/prefix fields".to_owned())
}

fn invalid_tar_path(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn put_octal(field: &mut [u8], value: u64) -> io::Result<()> {
    let text = format!("{value:o}");
    if text.len().saturating_add(1) > field.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "USTAR numeric field overflow",
        ));
    }
    field.fill(b'0');
    let terminator = field.len() - 1;
    let start = terminator - text.len();
    field[start..terminator].copy_from_slice(text.as_bytes());
    field[terminator] = 0;
    Ok(())
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(maximum.min(64 * 1024));
    File::open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > maximum {
        return Err(format!(
            "{} exceeds the {maximum}-byte limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn package_failure(path: &Path, message: &str, json: bool) -> CliOutput {
    failure(
        "package",
        3,
        vec![cli_diagnostic(
            "HYCEL-CLI-160",
            path.to_str(),
            "package",
            message,
        )],
        json,
    )
}
