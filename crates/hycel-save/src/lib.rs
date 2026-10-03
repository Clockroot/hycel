//! Bounded, versioned progress data stored in the user's local application-data directory.
//!
//! This crate persists only explicit stage/checkpoint progress, never arbitrary game objects.
//! Save I/O is external to `hycel-core`; hosts should save at an explicit boundary, not from a
//! simulation callback that may be retried or rolled back.

use std::{
    env,
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

/// Current progress-save schema version.
pub const SAVE_SCHEMA_VERSION: u32 = 1;
/// Maximum encoded primary or backup file size.
pub const MAX_SAVE_BYTES: usize = 1024 * 1024;
/// Maximum number of completed stages recorded in one save.
pub const MAX_COMPLETED_STAGES: usize = 256;

#[cfg(target_os = "windows")]
fn user_local_data_dir() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join("AppData").join("Local"))
        })
}

#[cfg(target_os = "macos")]
fn user_local_data_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("Library").join("Application Support"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn user_local_data_dir() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".local").join("share"))
        })
}

const SAVE_FILE_NAME: &str = "progress.json";
const BACKUP_FILE_NAME: &str = "progress.json.bak";
const CORRUPT_FILE_NAME: &str = "progress.json.corrupt";

/// Explicit, small progress state for a single-player game.
///
/// Stage identifiers are stable scene UUIDs. The optional checkpoint identifier is a stable
/// entity UUID within the current scene. The game host is responsible for resolving these
/// references against its authored content before applying them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameProgress {
    current_scene_id: String,
    checkpoint_entity_id: Option<String>,
    completed_scene_ids: Vec<String>,
}

impl GameProgress {
    /// Creates progress at a scene, with an optional scene-local checkpoint and completed scenes.
    ///
    /// Completed scene IDs are sorted into canonical order. Duplicate or malformed UUIDs and
    /// more than [`MAX_COMPLETED_STAGES`] entries are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`SaveError::InvalidData`] when an ID is malformed, duplicated, or over the
    /// completed-stage limit.
    pub fn new(
        current_scene_id: impl Into<String>,
        checkpoint_entity_id: Option<String>,
        mut completed_scene_ids: Vec<String>,
    ) -> Result<Self, SaveError> {
        let mut progress = Self {
            current_scene_id: current_scene_id.into(),
            checkpoint_entity_id,
            completed_scene_ids: {
                completed_scene_ids.sort();
                completed_scene_ids
            },
        };
        progress.validate()?;
        Ok(progress)
    }

    /// Current stage/scene UUID.
    #[must_use]
    pub fn current_scene_id(&self) -> &str {
        &self.current_scene_id
    }

    /// Current scene's checkpoint entity UUID, if any.
    #[must_use]
    pub fn checkpoint_entity_id(&self) -> Option<&str> {
        self.checkpoint_entity_id.as_deref()
    }

    /// Completed stage/scene UUIDs in canonical sorted order.
    #[must_use]
    pub fn completed_scene_ids(&self) -> &[String] {
        &self.completed_scene_ids
    }

    fn validate(&mut self) -> Result<(), SaveError> {
        validate_uuid(&self.current_scene_id).map_err(SaveError::InvalidData)?;
        if let Some(checkpoint_id) = &self.checkpoint_entity_id {
            validate_uuid(checkpoint_id).map_err(SaveError::InvalidData)?;
        }
        if self.completed_scene_ids.len() > MAX_COMPLETED_STAGES {
            return Err(SaveError::InvalidData(format!(
                "completed scene count exceeds the {MAX_COMPLETED_STAGES}-entry limit"
            )));
        }
        for scene_id in &self.completed_scene_ids {
            validate_uuid(scene_id).map_err(SaveError::InvalidData)?;
        }
        self.completed_scene_ids.sort();
        if self
            .completed_scene_ids
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(SaveError::InvalidData(
                "completed scene IDs must be unique".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Per-game store for one primary save and one last-known-good backup.
///
/// A store instance is intended to have one active writer. Each replacement is atomic, and
/// concurrent content changes detected between staging and commit are rejected. Callers should
/// still serialize writes from multiple game instances/processes.
#[derive(Clone, Debug)]
pub struct ProgressStore {
    game_id: String,
    directory: PathBuf,
    save_path: PathBuf,
    backup_path: PathBuf,
    corrupt_path: PathBuf,
}

impl ProgressStore {
    /// Opens the conventional per-user local-data location for `game_id`.
    ///
    /// Paths are rooted in the OS local application-data directory (XDG data home on Linux,
    /// Application Support on macOS, and Local `AppData` on Windows), then `hycel/<game UUID>/`.
    ///
    /// # Errors
    ///
    /// Returns an error if the ID is invalid or the OS cannot provide a user data directory.
    pub fn for_game(game_id: impl Into<String>) -> Result<Self, SaveError> {
        let game_id = game_id.into();
        validate_uuid(&game_id).map_err(SaveError::InvalidData)?;
        let base = user_local_data_dir().ok_or(SaveError::NoUserDataDirectory)?;
        Self::at_data_root(base, game_id)
    }

    /// Creates a store beneath an explicit local-data root.
    ///
    /// The per-game directory is `<data_root>/hycel/<game UUID>/`. This constructor supports
    /// hosts with an explicit path policy and isolated tests; the root is not created until a
    /// write or explicit backup recovery is requested.
    ///
    /// # Errors
    ///
    /// Returns an error if `game_id` is not a canonical lowercase UUID or `data_root` is not
    /// absolute.
    pub fn at_data_root(
        data_root: impl AsRef<Path>,
        game_id: impl Into<String>,
    ) -> Result<Self, SaveError> {
        let game_id = game_id.into();
        validate_uuid(&game_id).map_err(SaveError::InvalidData)?;
        let data_root = data_root.as_ref();
        if !data_root.is_absolute() {
            return Err(SaveError::InvalidData(
                "local data root must be an absolute path".to_owned(),
            ));
        }
        let directory = data_root.join("hycel").join(&game_id);
        Ok(Self {
            game_id,
            save_path: directory.join(SAVE_FILE_NAME),
            backup_path: directory.join(BACKUP_FILE_NAME),
            corrupt_path: directory.join(CORRUPT_FILE_NAME),
            directory,
        })
    }

    /// Primary save path, useful for diagnostics and user-facing settings.
    #[must_use]
    pub fn save_path(&self) -> &Path {
        &self.save_path
    }

    /// Last-known-good backup path.
    #[must_use]
    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    /// Loads and validates the primary save without modifying files.
    ///
    /// A missing primary save returns [`SaveError::NotFound`], even if a backup exists; callers
    /// must explicitly invoke [`Self::recover_backup`] to restore one.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the save is missing, invalid, incompatible, unsafe to read, or
    /// cannot be read from disk.
    pub fn load(&self) -> Result<GameProgress, SaveError> {
        if !self.inspect_directory()? {
            return Err(SaveError::NotFound);
        }
        let bytes = read_bounded_regular_file(&self.save_path)?.ok_or(SaveError::NotFound)?;
        self.decode(&bytes)
    }

    /// Writes progress atomically, first retaining the previous valid save as the backup.
    ///
    /// An invalid, unsupported, or foreign-game primary save is never overwritten. If the primary
    /// file is absent but a backup remains, callers must explicitly recover it before writing a
    /// new save. An existing backup is replaced only after the primary has been fully validated.
    ///
    /// # Errors
    ///
    /// Returns a typed error if progress is invalid, existing data is incompatible, a concurrent
    /// write is detected, or the backup/atomic write fails. Existing primary data is preserved
    /// when any validation or staging step fails.
    pub fn save(&self, progress: &GameProgress) -> Result<(), SaveError> {
        self.save_with(progress, || Ok(()))
    }

    fn save_with(
        &self,
        progress: &GameProgress,
        before_commit: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), SaveError> {
        let mut progress = progress.clone();
        progress.validate()?;
        let new_bytes = encode_document(&self.game_id, &progress)?;
        self.ensure_directory()?;

        let previous = read_bounded_regular_file(&self.save_path)?;
        if let Some(previous_bytes) = &previous {
            self.decode(previous_bytes)?;
            if let Some(backup_bytes) = read_bounded_regular_file(&self.backup_path)? {
                self.decode(&backup_bytes)?;
            }
            atomic_replace(&self.backup_path, previous_bytes)?;
        } else if read_bounded_regular_file(&self.backup_path)?.is_some() {
            return Err(SaveError::RecoveryRequired);
        }

        if read_bounded_regular_file(&self.save_path)? != previous {
            return Err(SaveError::ConcurrentModification);
        }
        before_commit().map_err(|error| {
            SaveError::io(
                "prepare progress save commit",
                self.save_path.clone(),
                error,
            )
        })?;
        atomic_replace(&self.save_path, &new_bytes)?;
        Ok(())
    }

    /// Explicitly restores a valid backup after primary corruption or loss.
    ///
    /// Bounded corrupt primary bytes are preserved as `progress.json.corrupt` before restoration;
    /// an existing file at that recovery path is never overwritten. Oversized/unreadable primaries
    /// fail closed and remain untouched. A valid primary, unsupported future schema, or backup
    /// with invalid/foreign data is not automatically downgraded.
    ///
    /// # Errors
    ///
    /// Returns a typed error when no valid backup exists, the primary is valid or belongs to a
    /// future schema, a corrupt save cannot be preserved, or restoration fails.
    pub fn recover_backup(&self) -> Result<GameProgress, SaveError> {
        self.ensure_directory()?;
        let backup_bytes =
            read_bounded_regular_file(&self.backup_path)?.ok_or(SaveError::BackupNotFound)?;
        let backup_progress = self.decode(&backup_bytes)?;

        let primary_bytes = read_bounded_regular_file(&self.save_path)?;
        if let Some(bytes) = &primary_bytes {
            match self.decode(bytes) {
                Ok(_) => return Err(SaveError::PrimaryIsValid),
                Err(SaveError::UnsupportedSchema(version)) => {
                    return Err(SaveError::RefuseFutureSchema(version));
                }
                Err(_) => preserve_new_file(&self.corrupt_path, bytes)?,
            }
        }
        if read_bounded_regular_file(&self.save_path)? != primary_bytes {
            return Err(SaveError::ConcurrentModification);
        }
        atomic_replace(&self.save_path, &backup_bytes)?;
        Ok(backup_progress)
    }

    fn inspect_directory(&self) -> Result<bool, SaveError> {
        let metadata = match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(SaveError::io(
                    "inspect per-game save directory",
                    self.directory.clone(),
                    error,
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SaveError::UnsafePath(self.directory.clone()));
        }
        Ok(true)
    }

    fn ensure_directory(&self) -> Result<(), SaveError> {
        fs::create_dir_all(&self.directory).map_err(|error| {
            SaveError::io(
                "create per-game save directory",
                self.directory.clone(),
                error,
            )
        })?;
        self.inspect_directory().map(|_| ())
    }

    fn decode(&self, bytes: &[u8]) -> Result<GameProgress, SaveError> {
        let document = decode_document(bytes)?;
        if document.game_id != self.game_id {
            return Err(SaveError::GameIdMismatch {
                expected: self.game_id.clone(),
                actual: document.game_id,
            });
        }
        let mut progress: GameProgress = document.progress.into();
        progress.validate()?;
        Ok(progress)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveDocument {
    schema_version: u32,
    game_id: String,
    progress: GameProgressWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GameProgressWire {
    current_scene_id: String,
    checkpoint_entity_id: Option<String>,
    completed_scene_ids: Vec<String>,
}

impl From<GameProgressWire> for GameProgress {
    fn from(value: GameProgressWire) -> Self {
        Self {
            current_scene_id: value.current_scene_id,
            checkpoint_entity_id: value.checkpoint_entity_id,
            completed_scene_ids: value.completed_scene_ids,
        }
    }
}

impl From<&GameProgress> for GameProgressWire {
    fn from(value: &GameProgress) -> Self {
        Self {
            current_scene_id: value.current_scene_id.clone(),
            checkpoint_entity_id: value.checkpoint_entity_id.clone(),
            completed_scene_ids: value.completed_scene_ids.clone(),
        }
    }
}

fn encode_document(game_id: &str, progress: &GameProgress) -> Result<Vec<u8>, SaveError> {
    let document = SaveDocument {
        schema_version: SAVE_SCHEMA_VERSION,
        game_id: game_id.to_owned(),
        progress: progress.into(),
    };
    let bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| SaveError::InvalidData(format!("cannot encode save: {error}")))?;
    if bytes.len() > MAX_SAVE_BYTES {
        return Err(SaveError::InvalidData(format!(
            "encoded save exceeds the {MAX_SAVE_BYTES}-byte limit"
        )));
    }
    Ok(bytes)
}

fn decode_document(bytes: &[u8]) -> Result<SaveDocument, SaveError> {
    if bytes.len() > MAX_SAVE_BYTES {
        return Err(SaveError::InvalidData(format!(
            "save exceeds the {MAX_SAVE_BYTES}-byte limit"
        )));
    }
    let header: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| SaveError::InvalidData(format!("invalid save JSON: {error}")))?;
    let schema_version = header
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            SaveError::InvalidData("save schema_version must be a positive integer".to_owned())
        })?;
    if schema_version != SAVE_SCHEMA_VERSION {
        return Err(SaveError::UnsupportedSchema(schema_version));
    }
    let document: SaveDocument = serde_json::from_slice(bytes)
        .map_err(|error| SaveError::InvalidData(format!("invalid save document: {error}")))?;
    if document.schema_version != SAVE_SCHEMA_VERSION {
        return Err(SaveError::UnsupportedSchema(document.schema_version));
    }
    let mut progress: GameProgress = document.progress.into();
    progress.validate()?;
    Ok(SaveDocument {
        schema_version: document.schema_version,
        game_id: document.game_id,
        progress: (&progress).into(),
    })
}

fn read_bounded_regular_file(path: &Path) -> Result<Option<Vec<u8>>, SaveError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(SaveError::io("inspect save file", path.to_owned(), error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SaveError::UnsafePath(path.to_owned()));
    }
    if metadata.len() > MAX_SAVE_BYTES as u64 {
        return Err(SaveError::InvalidData(format!(
            "{} exceeds the {MAX_SAVE_BYTES}-byte limit",
            path.display()
        )));
    }
    let file = File::open(path)
        .map_err(|error| SaveError::io("open save file", path.to_owned(), error))?;
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        SaveError::InvalidData("save size is not representable on this target".to_owned())
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take((MAX_SAVE_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| SaveError::io("read save file", path.to_owned(), error))?;
    if bytes.len() > MAX_SAVE_BYTES {
        return Err(SaveError::InvalidData(format!(
            "{} exceeds the {MAX_SAVE_BYTES}-byte limit",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    let mut file = AtomicWriteFile::open(path)
        .map_err(|error| SaveError::io("stage atomic save write", path.to_owned(), error))?;
    file.write_all(bytes)
        .map_err(|error| SaveError::io("write atomic save contents", path.to_owned(), error))?;
    file.commit()
        .map_err(|error| SaveError::io("commit atomic save write", path.to_owned(), error))
}

fn preserve_new_file(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| SaveError::io("preserve corrupt save", path.to_owned(), error))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(SaveError::io(
            "write preserved corrupt save",
            path.to_owned(),
            error,
        ));
    }
    Ok(())
}

fn validate_uuid(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || [8, 13, 18, 23]
            .into_iter()
            .any(|index| bytes[index] != b'-')
        || bytes.iter().enumerate().any(|(index, byte)| {
            index != 8
                && index != 13
                && index != 18
                && index != 23
                && !byte.is_ascii_digit()
                && !(b'a'..=b'f').contains(byte)
        })
    {
        return Err("identifier must be a canonical lowercase UUID".to_owned());
    }
    Ok(())
}

/// Typed failure from save validation, storage, or recovery.
#[derive(Debug)]
pub enum SaveError {
    /// Save contains invalid JSON, fields, identifiers, or progress values.
    InvalidData(String),
    /// Save schema is not understood by this engine version.
    UnsupportedSchema(u32),
    /// Save belongs to a different game/project UUID.
    GameIdMismatch { expected: String, actual: String },
    /// No primary save exists.
    NotFound,
    /// The OS could not provide the current user's local application-data path.
    NoUserDataDirectory,
    /// The primary file is absent while a backup exists; explicit recovery is required.
    RecoveryRequired,
    /// No backup file exists.
    BackupNotFound,
    /// A valid primary save exists, so explicit backup recovery is not appropriate.
    PrimaryIsValid,
    /// Explicit recovery would downgrade a primary written by a newer schema.
    RefuseFutureSchema(u32),
    /// Save changed between validation and replacement.
    ConcurrentModification,
    /// A save path resolved to a symlink or a non-regular file/directory.
    UnsafePath(PathBuf),
    /// An I/O operation failed.
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl SaveError {
    /// Stable diagnostic code for programmatic handling.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidData(_) => "HYCEL-SAVE-001",
            Self::UnsupportedSchema(_) | Self::RefuseFutureSchema(_) => "HYCEL-SAVE-002",
            Self::GameIdMismatch { .. } => "HYCEL-SAVE-003",
            Self::NotFound => "HYCEL-SAVE-004",
            Self::NoUserDataDirectory => "HYCEL-SAVE-005",
            Self::RecoveryRequired | Self::BackupNotFound | Self::PrimaryIsValid => {
                "HYCEL-SAVE-006"
            }
            Self::ConcurrentModification => "HYCEL-SAVE-007",
            Self::UnsafePath(_) => "HYCEL-SAVE-008",
            Self::Io { .. } => "HYCEL-SAVE-009",
        }
    }

    fn io(operation: &'static str, path: PathBuf, source: io::Error) -> Self {
        Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(message) => write!(f, "invalid progress save: {message}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported progress save schema {version}")
            }
            Self::GameIdMismatch { expected, actual } => write!(
                f,
                "progress save belongs to game {actual}, expected {expected}"
            ),
            Self::NotFound => write!(f, "progress save does not exist"),
            Self::NoUserDataDirectory => {
                write!(
                    f,
                    "the operating system did not provide a user data directory"
                )
            }
            Self::RecoveryRequired => write!(
                f,
                "a progress backup exists without a primary save; recover it explicitly before saving"
            ),
            Self::BackupNotFound => write!(f, "progress backup does not exist"),
            Self::PrimaryIsValid => write!(
                f,
                "primary progress save is valid; refusing backup recovery"
            ),
            Self::RefuseFutureSchema(version) => write!(
                f,
                "refusing to replace primary save schema {version} with an older backup"
            ),
            Self::ConcurrentModification => {
                write!(f, "progress save changed during the write operation")
            }
            Self::UnsafePath(path) => write!(
                f,
                "save path is a symlink or non-regular file: {}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "could not {operation} {}: {source}", path.display()),
        }
    }
}

impl Error for SaveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const GAME_ID: &str = "00000000-0000-4000-8000-000000000001";
    const SCENE_A: &str = "00000000-0000-4000-8000-000000000002";
    const SCENE_B: &str = "00000000-0000-4000-8000-000000000003";
    const CHECKPOINT: &str = "00000000-0000-4000-8000-000000000004";

    fn progress(scene_id: &str) -> GameProgress {
        GameProgress::new(
            scene_id,
            Some(CHECKPOINT.to_owned()),
            vec![SCENE_B.to_owned()],
        )
        .unwrap()
    }

    fn store() -> (ProgressStore, PathBuf) {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "hycel-save-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let store = ProgressStore::at_data_root(&root, GAME_ID).unwrap();
        (store, root)
    }

    #[test]
    fn progress_round_trips_in_strict_versioned_json() {
        let (store, root) = store();
        assert!(matches!(store.load(), Err(SaveError::NotFound)));
        let original = progress(SCENE_A);
        store.save(&original).unwrap();
        assert_eq!(store.load().unwrap(), original);
        let reopened = ProgressStore::at_data_root(&root, GAME_ID).unwrap();
        assert_eq!(reopened.load().unwrap(), original);
        assert!(
            store
                .save_path()
                .starts_with(root.join("hycel").join(GAME_ID))
        );
        let encoded = fs::read(store.save_path()).unwrap();
        assert!(
            String::from_utf8(encoded)
                .unwrap()
                .contains("\"schema_version\": 1")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_store_uses_per_game_path_in_user_local_data() {
        let store = ProgressStore::for_game(GAME_ID).unwrap();
        assert_eq!(store.save_path().file_name().unwrap(), SAVE_FILE_NAME);
        assert_eq!(store.backup_path().file_name().unwrap(), BACKUP_FILE_NAME);
        assert!(store.save_path().parent().unwrap().ends_with(GAME_ID));
        assert!(
            store
                .save_path()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .ends_with("hycel")
        );
    }

    #[test]
    fn rejects_unknown_duplicate_oversized_and_future_data_without_rewriting() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        let valid = fs::read(store.save_path()).unwrap();
        for invalid in [
            br#"{"schema_version":1,"game_id":"x","progress":{},"unknown":true}"#.to_vec(),
            br#"{"schema_version":1,"schema_version":1,"game_id":"x","progress":{}}"#.to_vec(),
            br#"{"schema_version":2,"game_id":"x","progress":{}}"#.to_vec(),
            vec![b' '; MAX_SAVE_BYTES + 1],
        ] {
            fs::write(store.save_path(), &invalid).unwrap();
            assert!(store.load().is_err());
            assert!(store.save(&progress(SCENE_B)).is_err());
            assert_eq!(fs::read(store.save_path()).unwrap(), invalid);
        }
        fs::write(store.save_path(), valid).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_rotates_one_valid_backup_and_rejects_foreign_ids() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        assert!(!store.backup_path().exists());
        store.save(&progress(SCENE_B)).unwrap();
        assert_eq!(store.load().unwrap(), progress(SCENE_B));
        assert_eq!(
            store
                .decode(&fs::read(store.backup_path()).unwrap())
                .unwrap(),
            progress(SCENE_A)
        );
        let mut foreign =
            encode_document("00000000-0000-4000-8000-000000000009", &progress(SCENE_A)).unwrap();
        fs::write(store.save_path(), &foreign).unwrap();
        assert!(matches!(
            store.load(),
            Err(SaveError::GameIdMismatch { .. })
        ));
        assert!(store.save(&progress(SCENE_A)).is_err());
        foreign.clear();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_save_keeps_previous_primary_and_last_known_good_backup() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        let error = store
            .save_with(&progress(SCENE_B), || {
                Err(io::Error::other("injected pre-commit failure"))
            })
            .unwrap_err();
        assert!(matches!(error, SaveError::Io { .. }));
        assert_eq!(store.load().unwrap(), progress(SCENE_A));
        assert_eq!(
            store
                .decode(&fs::read(store.backup_path()).unwrap())
                .unwrap(),
            progress(SCENE_A)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicitly_recovers_corrupt_primary_and_preserves_it_without_overwrite() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        store.save(&progress(SCENE_B)).unwrap();
        fs::write(store.save_path(), b"corrupt data").unwrap();
        let recovered = store.recover_backup().unwrap();
        assert_eq!(recovered, progress(SCENE_A));
        assert_eq!(store.load().unwrap(), progress(SCENE_A));
        assert_eq!(fs::read(&store.corrupt_path).unwrap(), b"corrupt data");
        fs::write(store.save_path(), b"second corruption").unwrap();
        assert!(matches!(store.recover_backup(), Err(SaveError::Io { .. })));
        assert_eq!(fs::read(store.save_path()).unwrap(), b"second corruption");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_backup_is_never_used_or_replaced_silently() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        store.save(&progress(SCENE_B)).unwrap();
        let primary = fs::read(store.save_path()).unwrap();
        fs::write(store.backup_path(), b"corrupt backup").unwrap();
        assert!(store.recover_backup().is_err());
        assert!(store.save(&progress(SCENE_A)).is_err());
        assert_eq!(fs::read(store.save_path()).unwrap(), primary);
        assert_eq!(fs::read(store.backup_path()).unwrap(), b"corrupt backup");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_primary_requires_explicit_backup_recovery() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        store.save(&progress(SCENE_B)).unwrap();
        fs::remove_file(store.save_path()).unwrap();
        assert!(matches!(store.load(), Err(SaveError::NotFound)));
        assert!(matches!(
            store.save(&progress(SCENE_B)),
            Err(SaveError::RecoveryRequired)
        ));
        assert_eq!(store.recover_backup().unwrap(), progress(SCENE_A));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_primary_is_preserved_and_requires_manual_recovery() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        store.save(&progress(SCENE_B)).unwrap();
        let oversized = vec![b'x'; MAX_SAVE_BYTES + 1];
        fs::write(store.save_path(), &oversized).unwrap();
        assert!(matches!(
            store.recover_backup(),
            Err(SaveError::InvalidData(_))
        ));
        assert_eq!(fs::read(store.save_path()).unwrap(), oversized);
        assert!(store.backup_path().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_to_downgrade_an_unsupported_primary_even_when_recovering() {
        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        store.save(&progress(SCENE_B)).unwrap();
        let future = br#"{"schema_version":9,"game_id":"00000000-0000-4000-8000-000000000001","progress":{}}"#;
        fs::write(store.save_path(), future).unwrap();
        assert!(matches!(
            store.recover_backup(),
            Err(SaveError::RefuseFutureSchema(9))
        ));
        assert_eq!(fs::read(store.save_path()).unwrap(), future);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_bad_ids_duplicate_completion_and_symlinks() {
        assert!(GameProgress::new("not-a-uuid", None, vec![]).is_err());
        assert!(GameProgress::new(SCENE_A, None, vec![SCENE_B.into(), SCENE_B.into()]).is_err());
        assert!(ProgressStore::at_data_root("unused", "../escape").is_err());
        assert!(ProgressStore::at_data_root("relative-root", GAME_ID).is_err());
        let boundary_ids: Vec<_> = (1..=MAX_COMPLETED_STAGES)
            .map(|index| format!("00000000-0000-4000-8000-{index:012x}"))
            .collect();
        assert!(GameProgress::new(SCENE_A, None, boundary_ids.clone()).is_ok());
        let mut too_many = boundary_ids;
        too_many.push("00000000-0000-4000-8000-000000000000".to_owned());
        assert!(GameProgress::new(SCENE_A, None, too_many).is_err());

        let (store, root) = store();
        store.save(&progress(SCENE_A)).unwrap();
        let elsewhere = root.join("outside.json");
        fs::write(&elsewhere, b"untouched").unwrap();
        fs::remove_file(store.save_path()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, store.save_path()).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&elsewhere, store.save_path()).unwrap();
        assert!(matches!(store.load(), Err(SaveError::UnsafePath(_))));
        assert!(matches!(
            store.save(&progress(SCENE_B)),
            Err(SaveError::UnsafePath(_))
        ));
        assert_eq!(fs::read(elsewhere).unwrap(), b"untouched");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exposes_stable_diagnostic_codes_and_rejects_file_paths_as_directories() {
        assert_eq!(SaveError::NotFound.code(), "HYCEL-SAVE-004");
        let (store, root) = store();
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(&root, b"not a directory").unwrap();
        assert!(store.save(&progress(SCENE_A)).is_err());
        fs::remove_file(root).unwrap();
    }
}
