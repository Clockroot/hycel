//! Failure-atomic scene schema migration and rollback helpers.
//!
//! Scene migrations preserve the original bytes in a numbered sibling backup
//! before an atomic replacement. Existing backups are never overwritten.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use serde_json::Value;

use super::{ComponentRegistry, Diagnostic, MAX_DOCUMENT_BYTES, SceneDocument, read_limited_file};

const CURRENT_SCENE_VERSION: u32 = 2;

/// Result of a successful migration or rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReceipt {
    /// Document path that was atomically replaced.
    pub path: PathBuf,
    /// Original bytes preserved before replacement.
    pub backup_path: PathBuf,
    /// Schema version before replacement.
    pub from_version: u32,
    /// Schema version after replacement.
    pub to_version: u32,
}

/// Migrates a valid scene JSON document from schema 1 to schema 2 in memory.
///
/// Schema 2 adds required entity `tags` arrays. The schema-1-to-2 migration
/// adds an empty array to each entity, preserving all schema-1 authored data.
/// Unknown fields and invalid source documents are rejected before conversion.
///
/// # Errors
///
/// Returns diagnostics if the source is invalid, already current, or uses an
/// unsupported schema version.
pub fn migrate_scene_json(
    input: &[u8],
    file: &str,
    registry: &ComponentRegistry,
) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let scene = SceneDocument::parse_json_with_registry(input, file, registry)?;
    if scene.schema_version() != 1 {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-001",
            Some(file),
            "$.schema_version",
            "only scene schema 1 can be migrated to schema 2",
        )]);
    }

    let mut document: Value = serde_json::from_slice(input).map_err(|error| {
        vec![Diagnostic::new(
            "HYCEL-MIGRATION-002",
            Some(file),
            "$",
            format!("cannot read validated scene JSON: {error}"),
        )]
    })?;
    let Some(object) = document.as_object_mut() else {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-002",
            Some(file),
            "$",
            "scene root must be a JSON object",
        )]);
    };
    object.insert(
        "schema_version".to_owned(),
        Value::Number(CURRENT_SCENE_VERSION.into()),
    );
    let Some(entities) = object.get_mut("entities").and_then(Value::as_array_mut) else {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-002",
            Some(file),
            "$.entities",
            "scene entities must be an array",
        )]);
    };
    for (index, entity) in entities.iter_mut().enumerate() {
        let Some(entity) = entity.as_object_mut() else {
            return Err(vec![Diagnostic::new(
                "HYCEL-MIGRATION-002",
                Some(file),
                format!("$.entities[{index}]"),
                "scene entity must be an object",
            )]);
        };
        entity.insert("tags".to_owned(), Value::Array(Vec::new()));
    }

    let migrated = serde_json::to_vec_pretty(&document).map_err(|error| {
        vec![Diagnostic::new(
            "HYCEL-MIGRATION-002",
            Some(file),
            "$",
            format!("cannot encode migrated scene: {error}"),
        )]
    })?;
    if migrated.len() > MAX_DOCUMENT_BYTES {
        return Err(vec![Diagnostic::new(
            "HYCEL-DOCUMENT-001",
            Some(file),
            "$",
            format!("migrated document exceeds the {MAX_DOCUMENT_BYTES}-byte limit"),
        )]);
    }
    SceneDocument::parse_json_with_registry(&migrated, file, registry)?;
    Ok(migrated)
}

/// Migrates a schema-1 scene file in place, preserving a numbered backup.
///
/// The scene is fully parsed, migrated, and validated before any filesystem
/// changes. The original is copied to a new sibling backup and synced before
/// the destination is atomically replaced. If the process stops before commit,
/// the original remains available and the backup can be retained safely.
/// The destination is rechecked immediately before commit to catch concurrent
/// content edits, though callers must still serialize against unrelated
/// writers. Calling this on a schema-2 file is an error; callers should inspect
/// its parsed schema version before requesting a migration.
///
/// # Errors
///
/// Returns diagnostics for invalid input, symlinks/non-regular/read-only
/// files, detected concurrent edits, backup failures, or an unsuccessful atomic
/// write. A backup may remain if the
/// atomic commit fails; the destination remains the original or the complete
/// migrated file, never a partial document.
pub fn migrate_scene_file(
    path: &Path,
    registry: &ComponentRegistry,
) -> Result<MigrationReceipt, Vec<Diagnostic>> {
    migrate_scene_file_with(path, registry, || Ok(()))
}

fn migrate_scene_file_with(
    path: &Path,
    registry: &ComponentRegistry,
    before_commit: impl FnOnce() -> std::io::Result<()>,
) -> Result<MigrationReceipt, Vec<Diagnostic>> {
    ensure_regular_non_symlink(path)?;
    let input = read_limited_file(path, MAX_DOCUMENT_BYTES).map_err(|error| {
        vec![Diagnostic::new(
            "HYCEL-MIGRATION-003",
            Some(&display_path(path)),
            "$",
            format!("cannot read scene before migration: {error}"),
        )]
    })?;
    let scene = SceneDocument::parse_json_with_registry(&input, &display_path(path), registry)?;
    let from_version = scene.schema_version();
    let output = migrate_scene_json(&input, &display_path(path), registry)?;
    let backup_path = backup_original(path, &input)?;
    if let Err(error) = write_atomically_with(path, &output, &input, before_commit) {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-004",
            Some(&display_path(path)),
            "$",
            format!(
                "atomic migration failed; original backup was preserved at {}: {error}",
                backup_path.display()
            ),
        )]);
    }
    Ok(MigrationReceipt {
        path: path.to_owned(),
        backup_path,
        from_version,
        to_version: CURRENT_SCENE_VERSION,
    })
}

/// Restores a validated scene backup, preserving the current scene as a new
/// numbered backup before atomically replacing it.
///
/// The backup must be a regular, non-symlink sibling file. Its contents are
/// validated as a supported scene before any write occurs. Read-only current
/// files are rejected, and destination bytes are rechecked immediately before
/// replacement.
///
/// # Errors
///
/// Returns diagnostics for invalid paths/data, or failures while backing up or
/// atomically replacing the current scene.
pub fn restore_scene_backup(
    path: &Path,
    backup_path: &Path,
    registry: &ComponentRegistry,
) -> Result<MigrationReceipt, Vec<Diagnostic>> {
    ensure_regular_non_symlink(path)?;
    ensure_regular_non_symlink(backup_path)?;
    if path.parent() != backup_path.parent() {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-005",
            Some(&display_path(path)),
            "$",
            "rollback backup must be a sibling of the scene file",
        )]);
    }
    let current = read_limited_file(path, MAX_DOCUMENT_BYTES).map_err(|error| {
        vec![io_diagnostic(
            path,
            "cannot read current scene for rollback",
            &error,
        )]
    })?;
    let backup = read_limited_file(backup_path, MAX_DOCUMENT_BYTES).map_err(|error| {
        vec![io_diagnostic(
            backup_path,
            "cannot read rollback backup",
            &error,
        )]
    })?;
    let current_scene =
        SceneDocument::parse_json_with_registry(&current, &display_path(path), registry)?;
    let backup_scene =
        SceneDocument::parse_json_with_registry(&backup, &display_path(backup_path), registry)?;
    let preserved_current = backup_original(path, &current)?;
    if let Err(error) = write_atomically(path, &backup, &current) {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-004",
            Some(&display_path(path)),
            "$",
            format!(
                "atomic rollback failed; current scene backup was preserved at {}: {error}",
                preserved_current.display()
            ),
        )]);
    }
    Ok(MigrationReceipt {
        path: path.to_owned(),
        backup_path: preserved_current,
        from_version: current_scene.schema_version(),
        to_version: backup_scene.schema_version(),
    })
}

fn ensure_regular_non_symlink(path: &Path) -> Result<(), Vec<Diagnostic>> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| vec![io_diagnostic(path, "cannot inspect scene path", &error)])?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-006",
            Some(&display_path(path)),
            "$",
            "migration path must be a regular, non-symlink file",
        )]);
    }
    if metadata.permissions().readonly() {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-009",
            Some(&display_path(path)),
            "$",
            "scene is read-only; clear its read-only permission explicitly before migration or rollback",
        )]);
    }
    Ok(())
}

fn backup_original(path: &Path, contents: &[u8]) -> Result<PathBuf, Vec<Diagnostic>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let Some(name) = path.file_name() else {
        return Err(vec![Diagnostic::new(
            "HYCEL-MIGRATION-005",
            Some(&display_path(path)),
            "$",
            "migration path must name a file",
        )]);
    };
    for sequence in 1_u32..=10_000 {
        let mut backup_name = OsString::from(name);
        backup_name.push(format!(".bak.{sequence}"));
        let candidate = parent.join(backup_name);
        let mut backup = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(vec![io_diagnostic(
                    &candidate,
                    "cannot create migration backup",
                    &error,
                )]);
            }
        };
        let result = backup.write_all(contents).and_then(|()| backup.sync_all());
        if let Err(error) = result {
            drop(backup);
            let cleanup = fs::remove_file(&candidate);
            let detail = cleanup.err().map_or_else(String::new, |cleanup_error| {
                format!("; incomplete backup cleanup also failed: {cleanup_error}")
            });
            return Err(vec![Diagnostic::new(
                "HYCEL-MIGRATION-007",
                Some(&display_path(&candidate)),
                "$",
                format!("cannot finish migration backup: {error}{detail}"),
            )]);
        }
        return Ok(candidate);
    }
    Err(vec![Diagnostic::new(
        "HYCEL-MIGRATION-008",
        Some(&display_path(path)),
        "$",
        "could not allocate a unique numbered backup name",
    )])
}

fn write_atomically(path: &Path, contents: &[u8], expected_original: &[u8]) -> std::io::Result<()> {
    write_atomically_with(path, contents, expected_original, || Ok(()))
}

fn write_atomically_with(
    path: &Path,
    contents: &[u8],
    expected_original: &[u8],
    before_commit: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let permissions = fs::metadata(path)?.permissions();
    let mut staged = AtomicWriteFile::open(path)?;
    staged.write_all(contents)?;
    staged.set_permissions(permissions)?;
    staged.sync_all()?;
    if let Err(error) = before_commit() {
        return discard_after_error(staged, error);
    }
    let current = match fs::read(path) {
        Ok(current) => current,
        Err(error) => return discard_after_error(staged, error),
    };
    if current != expected_original {
        return discard_after_error(
            staged,
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "destination changed during migration; refusing to overwrite concurrent edits",
            ),
        );
    }
    staged.commit()
}

fn discard_after_error(staged: AtomicWriteFile, error: std::io::Error) -> std::io::Result<()> {
    match staged.discard() {
        Ok(()) => Err(error),
        Err(cleanup_error) => Err(std::io::Error::new(
            error.kind(),
            format!("{error}; staged-file cleanup failed: {cleanup_error}"),
        )),
    }
}

fn io_diagnostic(path: &Path, message: &str, error: &std::io::Error) -> Diagnostic {
    Diagnostic::new(
        "HYCEL-MIGRATION-003",
        Some(&display_path(path)),
        "$",
        format!("{message}: {error}"),
    )
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        migrate_scene_file, migrate_scene_file_with, migrate_scene_json, restore_scene_backup,
    };
    use crate::{ComponentRegistry, SceneDocument};

    const SCENE_V1: &[u8] = br#"{
  "schema_version": 1,
  "id": "10000000-0000-4000-8000-000000000001",
  "name": "Room",
  "entities": [{"id":"20000000-0000-4000-8000-000000000001","name":"Player"}]
}"#;

    #[test]
    fn scene_v1_migrates_to_v2_and_preserves_authored_fields() {
        let migrated =
            migrate_scene_json(SCENE_V1, "room.json", &ComponentRegistry::default()).unwrap();
        let parsed = SceneDocument::parse_json(&migrated, "room.json").unwrap();
        assert_eq!(parsed.schema_version(), 2);
        assert_eq!(
            parsed.entities()[0].id(),
            "20000000-0000-4000-8000-000000000001"
        );
        assert_eq!(parsed.entities()[0].tags(), &[] as &[String]);
        assert_eq!(
            parsed.entities()[0].transform().scale_milli(),
            [1_000, 1_000]
        );
    }

    #[test]
    fn migration_rejects_unknown_fields_and_existing_schema_two() {
        let unknown = SCENE_V1.strip_suffix(b"}").unwrap();
        let unknown = [unknown, br#", "extra": true}"#].concat();
        assert_eq!(
            migrate_scene_json(&unknown, "room.json", &ComponentRegistry::default()).unwrap_err()
                [0]
            .code,
            "HYCEL-SCENE-001"
        );
        let v2 = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[]}"#;
        assert_eq!(
            migrate_scene_json(v2, "room.json", &ComponentRegistry::default()).unwrap_err()[0].code,
            "HYCEL-MIGRATION-001"
        );
        let null_tags = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","tags":null}]}"#;
        assert_eq!(
            migrate_scene_json(null_tags, "room.json", &ComponentRegistry::default()).unwrap_err()
                [0]
            .code,
            "HYCEL-SCENE-001"
        );
    }

    #[test]
    fn file_migration_keeps_numbered_backup_and_rollback_preserves_both_versions() {
        let directory = test_directory("migration-roundtrip");
        let scene_path = directory.join("room.json");
        fs::write(&scene_path, SCENE_V1).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&scene_path, fs::Permissions::from_mode(0o640)).unwrap();
        }

        let migrated = migrate_scene_file(&scene_path, &ComponentRegistry::default()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&scene_path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        assert_eq!((migrated.from_version, migrated.to_version), (1, 2));
        assert_eq!(fs::read(&migrated.backup_path).unwrap(), SCENE_V1);
        assert_eq!(
            SceneDocument::parse_json(&fs::read(&scene_path).unwrap(), "room.json")
                .unwrap()
                .schema_version(),
            2
        );

        let restored = restore_scene_backup(
            &scene_path,
            &migrated.backup_path,
            &ComponentRegistry::default(),
        )
        .unwrap();
        assert_eq!((restored.from_version, restored.to_version), (2, 1));
        assert_eq!(fs::read(&scene_path).unwrap(), SCENE_V1);
        assert_eq!(
            SceneDocument::parse_json(&fs::read(restored.backup_path).unwrap(), "backup.json")
                .unwrap()
                .schema_version(),
            2
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn migration_never_overwrites_an_existing_backup() {
        let directory = test_directory("migration-backup-collision");
        let scene_path = directory.join("room.json");
        let first_backup = directory.join("room.json.bak.1");
        fs::write(&scene_path, SCENE_V1).unwrap();
        fs::write(&first_backup, b"user-owned backup").unwrap();

        let receipt = migrate_scene_file(&scene_path, &ComponentRegistry::default()).unwrap();
        assert_eq!(receipt.backup_path, directory.join("room.json.bak.2"));
        assert_eq!(fs::read(first_backup).unwrap(), b"user-owned backup");
        assert_eq!(fs::read(&receipt.backup_path).unwrap(), SCENE_V1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn interrupted_migration_leaves_original_and_backup_intact() {
        let directory = test_directory("migration-interrupted");
        let scene_path = directory.join("room.json");
        fs::write(&scene_path, SCENE_V1).unwrap();
        let error = migrate_scene_file_with(&scene_path, &ComponentRegistry::default(), || {
            Err(std::io::Error::other("injected interruption before commit"))
        })
        .unwrap_err();

        assert_eq!(error[0].code, "HYCEL-MIGRATION-004");
        assert_eq!(fs::read(&scene_path).unwrap(), SCENE_V1);
        assert_eq!(
            fs::read(directory.join("room.json.bak.1")).unwrap(),
            SCENE_V1
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn migration_aborts_if_destination_changes_before_commit() {
        let directory = test_directory("migration-concurrent-edit");
        let scene_path = directory.join("room.json");
        let concurrent_edit = b"external editor content";
        fs::write(&scene_path, SCENE_V1).unwrap();

        let error = migrate_scene_file_with(&scene_path, &ComponentRegistry::default(), || {
            fs::write(&scene_path, concurrent_edit)
        })
        .unwrap_err();
        assert_eq!(error[0].code, "HYCEL-MIGRATION-004");
        assert_eq!(fs::read(&scene_path).unwrap(), concurrent_edit);
        assert_eq!(
            fs::read(directory.join("room.json.bak.1")).unwrap(),
            SCENE_V1
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)] // Non-Unix cleanup only clears the test's readonly attribute.
    fn migration_refuses_read_only_destination_without_changing_it() {
        let directory = test_directory("migration-read-only");
        let scene_path = directory.join("room.json");
        fs::write(&scene_path, SCENE_V1).unwrap();
        let mut permissions = fs::metadata(&scene_path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&scene_path, permissions).unwrap();

        assert_eq!(
            migrate_scene_file(&scene_path, &ComponentRegistry::default()).unwrap_err()[0].code,
            "HYCEL-MIGRATION-009"
        );
        assert_eq!(fs::read(&scene_path).unwrap(), SCENE_V1);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&scene_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(&scene_path).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&scene_path, permissions).unwrap();
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn migration_refuses_symlink_destination() {
        use std::os::unix::fs::symlink;

        let directory = test_directory("migration-symlink");
        let target = directory.join("target.json");
        let link = directory.join("room.json");
        fs::write(&target, SCENE_V1).unwrap();
        symlink(&target, &link).unwrap();
        assert_eq!(
            migrate_scene_file(&link, &ComponentRegistry::default()).unwrap_err()[0].code,
            "HYCEL-MIGRATION-006"
        );
        assert_eq!(fs::read(&target).unwrap(), SCENE_V1);
        fs::remove_dir_all(directory).unwrap();
    }

    fn test_directory(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hycel-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
