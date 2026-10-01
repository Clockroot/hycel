//! Deterministic asset content identity, import fingerprints, and dependency reports.
//!
//! Authored resource UUIDs remain stable project identities. Source SHA-256
//! digests and importer fingerprints describe content/tool inputs independently;
//! generated import records are organized by resource UUID and fingerprint.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hycel_project::{
    ComponentRegistry, Diagnostic, ResourceDescriptor, SceneDocument,
    resolve_existing_project_path, validate_relative_project_path,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const IMPORT_FINGERPRINT_DOMAIN: &[u8] = b"hycel.import-fingerprint.v1\0";
const HASH_BUFFER_BYTES: usize = 64 * 1024;
/// Maximum serialized import-record size accepted by the metadata reader.
pub const MAX_IMPORT_RECORD_BYTES: usize = 1024 * 1024;

/// SHA-256 digest of authored source bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Computes the digest of a byte slice.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Returns the 32-byte digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hexadecimal digest.
    #[must_use]
    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }
}

/// Stable content fingerprint for one importer invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportFingerprint([u8; 32]);

impl ImportFingerprint {
    /// Computes a versioned fingerprint from source bytes, importer kind,
    /// importer/tool version, and canonical JSON import settings.
    ///
    /// The authored resource UUID and source path are intentionally excluded.
    /// They are retained separately in [`ImportRecord`] and in the UUID-based
    /// cache location. Consequently, changing content or importer inputs makes
    /// a new cache entry, while moving identical source bytes does not.
    ///
    /// # Errors
    ///
    /// Returns an error if the importer version is empty/oversized or settings
    /// cannot be encoded as JSON.
    pub fn compute(
        source_hash: ContentHash,
        importer_kind: &str,
        importer_version: &str,
        settings: &BTreeMap<String, Value>,
    ) -> Result<Self, AssetError> {
        validate_version(importer_version)?;
        let settings = serde_json::to_vec(settings).map_err(AssetError::SettingsEncoding)?;
        let mut hasher = Sha256::new();
        hasher.update(IMPORT_FINGERPRINT_DOMAIN);
        hasher.update(source_hash.as_bytes());
        hash_length_prefixed(&mut hasher, importer_kind.as_bytes());
        hash_length_prefixed(&mut hasher, importer_version.as_bytes());
        hash_length_prefixed(&mut hasher, &settings);
        Ok(Self(hasher.finalize().into()))
    }

    /// Lowercase hexadecimal fingerprint.
    #[must_use]
    pub fn to_hex(self) -> String {
        encode_hex(&self.0)
    }
}

/// Versioned metadata describing the exact inputs used for an import.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportRecord {
    schema_version: u32,
    resource_id: String,
    source_path: String,
    source_sha256: String,
    importer_kind: String,
    importer_version: String,
    settings: BTreeMap<String, Value>,
    fingerprint_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRecordWire {
    schema_version: u32,
    resource_id: String,
    source_path: String,
    source_sha256: String,
    importer_kind: String,
    importer_version: String,
    settings: BTreeMap<String, Value>,
    fingerprint_sha256: String,
}

impl ImportRecord {
    /// Captures resource identity and all importer inputs in a serializable record.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied importer version is invalid or its
    /// settings cannot be encoded.
    pub fn new(
        resource: &ResourceDescriptor,
        source_hash: ContentHash,
        importer_version: &str,
    ) -> Result<Self, AssetError> {
        let fingerprint = ImportFingerprint::compute(
            source_hash,
            resource.kind(),
            importer_version,
            resource.import_settings(),
        )?;
        Ok(Self {
            schema_version: 1,
            resource_id: resource.id().to_owned(),
            source_path: resource.source().to_owned(),
            source_sha256: source_hash.to_hex(),
            importer_kind: resource.kind().to_owned(),
            importer_version: importer_version.to_owned(),
            settings: resource.import_settings().clone(),
            fingerprint_sha256: fingerprint.to_hex(),
        })
    }

    /// Import metadata schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Stable authored resource UUID.
    #[must_use]
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    /// Project-relative authored source path.
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// SHA-256 of the complete source file bytes.
    #[must_use]
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    /// Importer implementation/version used for this record.
    #[must_use]
    pub fn importer(&self) -> (&str, &str) {
        (&self.importer_kind, &self.importer_version)
    }

    /// Settings snapshot in lexical key order.
    #[must_use]
    pub fn settings(&self) -> &BTreeMap<String, Value> {
        &self.settings
    }

    /// SHA-256 import fingerprint.
    #[must_use]
    pub fn fingerprint_sha256(&self) -> &str {
        &self.fingerprint_sha256
    }

    /// UUID-organized cache directory, with the content/tool fingerprint kept
    /// as a separate versioned identity.
    #[must_use]
    pub fn cache_relative_path(&self) -> PathBuf {
        Path::new(".hycel")
            .join("imports")
            .join(&self.resource_id)
            .join(&self.fingerprint_sha256)
    }

    /// Compact deterministic JSON representation of this record.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the record cannot be represented as JSON.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Parses a bounded, strict import record and verifies its stored fingerprint.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized/malformed metadata, unknown fields,
    /// invalid IDs/paths/hashes, unsupported versions, or inconsistent inputs.
    pub fn parse_json(input: &[u8]) -> Result<Self, AssetError> {
        if input.len() > MAX_IMPORT_RECORD_BYTES {
            return Err(AssetError::ImportRecordTooLarge);
        }
        let wire: ImportRecordWire =
            serde_json::from_slice(input).map_err(AssetError::InvalidImportRecord)?;
        let record = Self {
            schema_version: wire.schema_version,
            resource_id: wire.resource_id,
            source_path: wire.source_path,
            source_sha256: wire.source_sha256,
            importer_kind: wire.importer_kind,
            importer_version: wire.importer_version,
            settings: wire.settings,
            fingerprint_sha256: wire.fingerprint_sha256,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<(), AssetError> {
        if self.schema_version != 1 {
            return Err(AssetError::UnsupportedImportRecordVersion(
                self.schema_version,
            ));
        }
        if !valid_uuid(&self.resource_id) {
            return Err(AssetError::InvalidImportRecordValue("resource_id"));
        }
        validate_relative_project_path(&self.source_path)
            .map_err(AssetError::InvalidProjectPath)?;
        if !valid_importer_kind(&self.importer_kind) {
            return Err(AssetError::InvalidImportRecordValue("importer_kind"));
        }
        validate_version(&self.importer_version)?;
        let source_hash = decode_hex_32(&self.source_sha256)
            .ok_or(AssetError::InvalidImportRecordValue("source_sha256"))?;
        let recorded_fingerprint = decode_hex_32(&self.fingerprint_sha256)
            .ok_or(AssetError::InvalidImportRecordValue("fingerprint_sha256"))?;
        let computed = ImportFingerprint::compute(
            ContentHash(source_hash),
            &self.importer_kind,
            &self.importer_version,
            &self.settings,
        )?;
        if computed.0 != recorded_fingerprint {
            return Err(AssetError::FingerprintMismatch);
        }
        Ok(())
    }
}

/// Why a cached import must be regenerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReimportReason {
    /// No previous validated import record exists.
    FirstImport,
    /// The resource's stable UUID changed.
    ResourceIdentityChanged,
    /// Authored source content changed.
    SourceChanged,
    /// Importer kind or tool version changed.
    ImporterChanged,
    /// Import settings changed.
    SettingsChanged,
    /// Fingerprint schema/algorithm changed despite equal current inputs.
    FingerprintChanged,
}

/// Deterministic decision comparing a prior import record with desired inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReimportDecision {
    /// No cached output may be reused; import with the stated reason.
    Import(ReimportReason),
    /// Inputs and authored source path are unchanged; reuse cached output.
    ReuseCachedOutput,
    /// Import output is identical, but stored source-path metadata should update.
    UpdateMetadataOnly,
}

/// Chooses whether cached output can be reused, without consulting timestamps.
#[must_use]
pub fn plan_reimport(previous: Option<&ImportRecord>, desired: &ImportRecord) -> ReimportDecision {
    let Some(previous) = previous else {
        return ReimportDecision::Import(ReimportReason::FirstImport);
    };
    if previous.resource_id != desired.resource_id {
        return ReimportDecision::Import(ReimportReason::ResourceIdentityChanged);
    }
    if previous.source_sha256 != desired.source_sha256 {
        return ReimportDecision::Import(ReimportReason::SourceChanged);
    }
    if previous.importer_kind != desired.importer_kind
        || previous.importer_version != desired.importer_version
    {
        return ReimportDecision::Import(ReimportReason::ImporterChanged);
    }
    if previous.settings != desired.settings {
        return ReimportDecision::Import(ReimportReason::SettingsChanged);
    }
    if previous.fingerprint_sha256 != desired.fingerprint_sha256 {
        return ReimportDecision::Import(ReimportReason::FingerprintChanged);
    }
    if previous.source_path != desired.source_path {
        return ReimportDecision::UpdateMetadataOnly;
    }
    ReimportDecision::ReuseCachedOutput
}

/// Hashes a project-relative authored source file without loading it into memory.
///
/// The path is validated/resolved against `project_root`, including symlink
/// containment, and must resolve to a regular file. `max_bytes` is an explicit
/// caller policy; the function streams at most that many bytes plus one byte to
/// detect an oversized source. Timestamps and filesystem traversal order never
/// enter the digest. This is not a filesystem snapshot; callers must serialize
/// source writes while hashing to avoid observing a concurrent partial edit.
///
/// # Errors
///
/// Returns an error for an invalid/escaping path, non-file source, I/O failure,
/// or a source larger than `max_bytes`.
pub fn hash_source_file(
    project_root: &Path,
    relative_path: &str,
    max_bytes: u64,
) -> Result<ContentHash, AssetError> {
    let path = resolve_existing_project_path(project_root, relative_path)
        .map_err(AssetError::InvalidProjectPath)?;
    let metadata = std::fs::metadata(&path).map_err(|source| AssetError::Io {
        path: relative_path.to_owned(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(AssetError::NotRegularFile(relative_path.to_owned()));
    }
    if metadata.len() > max_bytes {
        return Err(AssetError::SourceTooLarge {
            path: relative_path.to_owned(),
            max_bytes,
        });
    }
    let file = File::open(&path).map_err(|source| AssetError::Io {
        path: relative_path.to_owned(),
        source,
    })?;
    hash_reader(file, relative_path, max_bytes)
}

fn hash_reader(reader: impl Read, path: &str, max_bytes: u64) -> Result<ContentHash, AssetError> {
    let mut reader = reader.take(max_bytes.saturating_add(1));
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|source| AssetError::Io {
            path: path.to_owned(),
            source,
        })?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| AssetError::SourceTooLarge {
                path: path.to_owned(),
                max_bytes,
            })?;
        if total > max_bytes {
            return Err(AssetError::SourceTooLarge {
                path: path.to_owned(),
                max_bytes,
            });
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ContentHash(hasher.finalize().into()))
}

/// One project's resource and scene-to-resource dependency report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetDependencyReport {
    schema_version: u32,
    resources: BTreeMap<String, ResourceDependency>,
    scenes: BTreeMap<String, SceneDependency>,
}

/// A resource's authored source dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceDependency {
    resource_id: String,
    kind: String,
    descriptor_file: String,
    source_path: String,
}

/// A scene's direct resource dependencies, in lexical UUID order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SceneDependency {
    scene_id: String,
    scene_file: String,
    resource_ids: Vec<String>,
}

impl AssetDependencyReport {
    /// Builds a deterministic dependency report from already parsed documents.
    ///
    /// Scene references are discovered only from resource-reference fields
    /// declared in the component registry. Input tuples retain project-relative
    /// file context. Callers should validate project documents first to obtain
    /// the normal detailed dangling-reference diagnostics.
    ///
    /// # Errors
    ///
    /// Returns errors for duplicate resource identities or references to absent
    /// resource descriptors.
    pub fn build(
        scenes: &[(String, SceneDocument)],
        resources: &[(String, ResourceDescriptor)],
        registry: &ComponentRegistry,
    ) -> Result<Self, AssetError> {
        let mut resource_map = BTreeMap::new();
        for (descriptor_file, resource) in resources {
            validate_relative_project_path(descriptor_file)
                .map_err(AssetError::InvalidProjectPath)?;
            let entry = ResourceDependency {
                resource_id: resource.id().to_owned(),
                kind: resource.kind().to_owned(),
                descriptor_file: descriptor_file.clone(),
                source_path: resource.source().to_owned(),
            };
            if resource_map
                .insert(resource.id().to_owned(), entry)
                .is_some()
            {
                return Err(AssetError::DuplicateResource(resource.id().to_owned()));
            }
        }
        let mut scene_map = BTreeMap::new();
        for (file, scene) in scenes {
            validate_relative_project_path(file).map_err(AssetError::InvalidProjectPath)?;
            let resource_ids = scene.referenced_resource_ids(registry);
            for resource_id in &resource_ids {
                if !resource_map.contains_key(resource_id) {
                    return Err(AssetError::MissingResource {
                        scene_id: scene.id().to_owned(),
                        resource_id: resource_id.clone(),
                    });
                }
            }
            let entry = SceneDependency {
                scene_id: scene.id().to_owned(),
                scene_file: file.clone(),
                resource_ids: resource_ids.into_iter().collect(),
            };
            if scene_map.insert(scene.id().to_owned(), entry).is_some() {
                return Err(AssetError::DuplicateScene(scene.id().to_owned()));
            }
        }
        Ok(Self {
            schema_version: 1,
            resources: resource_map,
            scenes: scene_map,
        })
    }

    /// Dependency report schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Resource entries keyed by stable UUID.
    #[must_use]
    pub fn resources(&self) -> &BTreeMap<String, ResourceDependency> {
        &self.resources
    }

    /// Scene entries keyed by stable scene UUID.
    #[must_use]
    pub fn scenes(&self) -> &BTreeMap<String, SceneDependency> {
        &self.scenes
    }

    /// Deterministic JSON output.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if the report cannot be represented as JSON.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

impl ResourceDependency {
    /// Stable resource UUID.
    #[must_use]
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }

    /// Registered importer kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Project-relative descriptor file.
    #[must_use]
    pub fn descriptor_file(&self) -> &str {
        &self.descriptor_file
    }

    /// Project-relative authored input.
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }
}

impl SceneDependency {
    /// Stable scene UUID.
    #[must_use]
    pub fn scene_id(&self) -> &str {
        &self.scene_id
    }

    /// Project-relative scene file.
    #[must_use]
    pub fn scene_file(&self) -> &str {
        &self.scene_file
    }

    /// Direct resources referenced by registered component fields.
    #[must_use]
    pub fn resource_ids(&self) -> &[String] {
        &self.resource_ids
    }
}

/// Errors from content hashing, fingerprints, and dependency reports.
#[derive(Debug)]
pub enum AssetError {
    /// Project path failed validation or escaped the root.
    InvalidProjectPath(Diagnostic),
    /// Source was a directory or another non-regular file.
    NotRegularFile(String),
    /// Source exceeded the caller-provided size bound.
    SourceTooLarge { path: String, max_bytes: u64 },
    /// File-system operation failed for a project-relative path.
    Io { path: String, source: io::Error },
    /// Importer/tool version is empty, too long, or contains control characters.
    InvalidImporterVersion,
    /// Import record exceeded the configured bound.
    ImportRecordTooLarge,
    /// Import record JSON is malformed or contains unknown fields.
    InvalidImportRecord(serde_json::Error),
    /// Import record schema is not supported.
    UnsupportedImportRecordVersion(u32),
    /// A validated import record field has an invalid value.
    InvalidImportRecordValue(&'static str),
    /// Recorded source/settings/tool inputs do not match the stored fingerprint.
    FingerprintMismatch,
    /// Import settings could not be encoded deterministically.
    SettingsEncoding(serde_json::Error),
    /// A resource UUID appears more than once in the dependency report.
    DuplicateResource(String),
    /// A scene UUID appears more than once in the dependency report.
    DuplicateScene(String),
    /// A scene references a resource UUID without a descriptor.
    MissingResource {
        scene_id: String,
        resource_id: String,
    },
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProjectPath(error) => write!(f, "{error}"),
            Self::NotRegularFile(path) => write!(f, "asset source is not a regular file: {path}"),
            Self::SourceTooLarge { path, max_bytes } => {
                write!(f, "asset source exceeds {max_bytes} bytes: {path}")
            }
            Self::Io { path, source } => write!(f, "asset source I/O failed for {path}: {source}"),
            Self::InvalidImporterVersion => write!(
                f,
                "importer/tool version must contain 1–128 non-control UTF-8 bytes"
            ),
            Self::ImportRecordTooLarge => write!(
                f,
                "import record exceeds the {MAX_IMPORT_RECORD_BYTES}-byte limit"
            ),
            Self::InvalidImportRecord(error) => write!(f, "invalid import record JSON: {error}"),
            Self::UnsupportedImportRecordVersion(version) => {
                write!(f, "unsupported import record schema version: {version}")
            }
            Self::InvalidImportRecordValue(field) => {
                write!(f, "invalid import record field: {field}")
            }
            Self::FingerprintMismatch => write!(
                f,
                "import record fingerprint does not match its source/settings/tool inputs"
            ),
            Self::SettingsEncoding(error) => write!(f, "cannot encode import settings: {error}"),
            Self::DuplicateResource(id) => {
                write!(f, "duplicate resource UUID in dependency report: {id}")
            }
            Self::DuplicateScene(id) => {
                write!(f, "duplicate scene UUID in dependency report: {id}")
            }
            Self::MissingResource {
                scene_id,
                resource_id,
            } => write!(
                f,
                "scene {scene_id} references missing resource {resource_id}"
            ),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidProjectPath(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::SettingsEncoding(error) | Self::InvalidImportRecord(error) => Some(error),
            _ => None,
        }
    }
}

fn valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && ![8, 13, 18, 23]
            .into_iter()
            .any(|index| bytes[index] != b'-')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
}

fn valid_importer_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn decode_hex_32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let high = hex.as_bytes()[index * 2];
        let low = hex.as_bytes()[index * 2 + 1];
        *byte = (hex_nibble(high)? << 4) | hex_nibble(low)?;
    }
    Some(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn validate_version(version: &str) -> Result<(), AssetError> {
    if version.is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
        Err(AssetError::InvalidImporterVersion)
    } else {
        Ok(())
    }
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hycel_project::{ComponentRegistry, ResourceDescriptor, ResourceRegistry, SceneDocument};
    use serde_json::json;
    use sha2::Digest;

    use super::{
        AssetDependencyReport, AssetError, ContentHash, ImportFingerprint, ImportRecord,
        ReimportDecision, ReimportReason, hash_source_file, plan_reimport,
    };

    const RESOURCE: &[u8] = br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"texture","source":"assets/player.png","import":{"mipmaps":true,"quality":85}}"#;
    const SCENE: &[u8] = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"Player","tags":["player"],"components":[{"type":"hycel.sprite","schema_version":1,"data":{"texture":"30000000-0000-4000-8000-000000000001"}}]}]}"#;

    #[test]
    fn source_hash_is_sha256_and_enforces_caller_limit() {
        let directory = test_directory("asset-hash");
        let root = directory.join("project");
        fs::create_dir_all(root.join("assets")).unwrap();
        let source = b"hycel asset bytes";
        fs::write(root.join("assets/player.png"), source).unwrap();

        let digest = hash_source_file(&root, "assets/player.png", 128).unwrap();
        assert_eq!(
            digest.to_hex(),
            format!("{:x}", sha2::Sha256::digest(source))
        );
        assert!(matches!(
            hash_source_file(&root, "assets/player.png", 2),
            Err(AssetError::SourceTooLarge { .. })
        ));
        assert!(matches!(
            hash_source_file(&root, "../outside", 128),
            Err(AssetError::InvalidProjectPath(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fingerprint_is_stable_and_changes_for_source_settings_or_tool_version() {
        let source = ContentHash::from_bytes(b"same source");
        let first_settings = [("quality".to_owned(), json!(80))].into_iter().collect();
        let reordered_settings = [("quality".to_owned(), json!(80))].into_iter().collect();
        let base =
            ImportFingerprint::compute(source, "texture", "png-1.0", &first_settings).unwrap();
        assert_eq!(
            base,
            ImportFingerprint::compute(source, "texture", "png-1.0", &reordered_settings).unwrap()
        );
        assert_ne!(
            base,
            ImportFingerprint::compute(
                ContentHash::from_bytes(b"changed"),
                "texture",
                "png-1.0",
                &first_settings
            )
            .unwrap()
        );
        assert_ne!(
            base,
            ImportFingerprint::compute(
                source,
                "texture",
                "png-1.0",
                &[("quality".to_owned(), json!(90))].into_iter().collect()
            )
            .unwrap()
        );
        assert_ne!(
            base,
            ImportFingerprint::compute(source, "texture", "png-2.0", &first_settings).unwrap()
        );
        assert_eq!(base.to_hex().len(), 64);
    }

    #[test]
    fn import_record_keeps_uuid_separate_from_fingerprint_and_uses_hybrid_cache_path() {
        let resource = parsed_resource();
        let record = ImportRecord::new(
            &resource,
            ContentHash::from_bytes(b"source"),
            "png-tool-1.2",
        )
        .unwrap();
        assert_eq!(record.schema_version(), 1);
        assert_eq!(record.resource_id(), resource.id());
        assert_eq!(record.importer(), ("texture", "png-tool-1.2"));
        assert!(
            record
                .cache_relative_path()
                .starts_with(".hycel/imports/30000000-0000-4000-8000-000000000001")
        );
        assert!(
            record
                .to_json()
                .unwrap()
                .windows(b"quality".len())
                .any(|window| window == b"quality")
        );
    }

    #[test]
    fn import_records_round_trip_and_verify_fingerprint_inputs() {
        let resource = parsed_resource();
        let record = ImportRecord::new(
            &resource,
            ContentHash::from_bytes(b"source"),
            "png-tool-1.2",
        )
        .unwrap();
        let encoded = record.to_json().unwrap();
        assert_eq!(ImportRecord::parse_json(&encoded).unwrap(), record);

        let mut tampered: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        tampered["settings"]["quality"] = json!(10);
        let changed = serde_json::to_vec(&tampered).unwrap();
        assert!(matches!(
            ImportRecord::parse_json(&changed),
            Err(AssetError::FingerprintMismatch)
        ));

        let mut escaped: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        escaped["resource_id"] = json!("../../outside");
        let escaped = serde_json::to_vec(&escaped).unwrap();
        assert!(matches!(
            ImportRecord::parse_json(&escaped),
            Err(AssetError::InvalidImportRecordValue("resource_id"))
        ));
    }

    #[test]
    fn reimport_policy_is_fingerprint_based_and_path_moves_only_update_metadata() {
        let original = parsed_resource();
        let source_hash = ContentHash::from_bytes(b"same source");
        let prior = ImportRecord::new(&original, source_hash, "png-tool-1.2").unwrap();
        assert_eq!(
            plan_reimport(None, &prior),
            ReimportDecision::Import(ReimportReason::FirstImport)
        );
        assert_eq!(
            plan_reimport(Some(&prior), &prior),
            ReimportDecision::ReuseCachedOutput
        );

        let moved_data = String::from_utf8(RESOURCE.to_vec())
            .unwrap()
            .replace("assets/player.png", "assets/renamed.png");
        let moved = ResourceDescriptor::parse_json_with_registry(
            moved_data.as_bytes(),
            "assets/renamed.png.hycel.json",
            &resource_registry(),
        )
        .unwrap();
        let moved_record = ImportRecord::new(&moved, source_hash, "png-tool-1.2").unwrap();
        assert_eq!(
            plan_reimport(Some(&prior), &moved_record),
            ReimportDecision::UpdateMetadataOnly
        );

        let changed_source = ImportRecord::new(
            &original,
            ContentHash::from_bytes(b"new bytes"),
            "png-tool-1.2",
        )
        .unwrap();
        assert_eq!(
            plan_reimport(Some(&prior), &changed_source),
            ReimportDecision::Import(ReimportReason::SourceChanged)
        );
    }

    #[test]
    fn dependency_report_is_sorted_and_resolves_scene_resource_edges() {
        let mut registry = ComponentRegistry::default();
        registry
            .register("hycel.sprite", 1, false, ["texture"])
            .unwrap();
        registry
            .mark_resource_reference("hycel.sprite", "texture")
            .unwrap();
        let scene =
            SceneDocument::parse_json_with_registry(SCENE, "scenes/room.json", &registry).unwrap();
        let resource = parsed_resource();
        let report = AssetDependencyReport::build(
            &[("scenes/room.json".to_owned(), scene)],
            &[("assets/player.png.hycel.json".to_owned(), resource)],
            &registry,
        )
        .unwrap();
        assert_eq!(report.schema_version(), 1);
        assert_eq!(
            report
                .resources()
                .values()
                .next()
                .unwrap()
                .descriptor_file(),
            "assets/player.png.hycel.json"
        );
        assert_eq!(
            report.scenes().values().next().unwrap().resource_ids(),
            &["30000000-0000-4000-8000-000000000001"]
        );
        assert!(
            String::from_utf8(report.to_json().unwrap())
                .unwrap()
                .contains("assets/player.png")
        );
    }

    #[test]
    fn dependency_report_rejects_dangling_resource_ids() {
        let mut registry = ComponentRegistry::default();
        registry
            .register("hycel.sprite", 1, false, ["texture"])
            .unwrap();
        registry
            .mark_resource_reference("hycel.sprite", "texture")
            .unwrap();
        let scene =
            SceneDocument::parse_json_with_registry(SCENE, "scenes/room.json", &registry).unwrap();
        assert!(matches!(
            AssetDependencyReport::build(&[("scenes/room.json".to_owned(), scene)], &[], &registry),
            Err(AssetError::MissingResource { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn source_hash_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = test_directory("asset-symlink");
        let root = directory.join("project");
        fs::create_dir_all(root.join("assets")).unwrap();
        let outside = directory.join("outside.bin");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, root.join("assets/escape.bin")).unwrap();
        assert!(matches!(
            hash_source_file(&root, "assets/escape.bin", 128),
            Err(AssetError::InvalidProjectPath(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    fn parsed_resource() -> ResourceDescriptor {
        ResourceDescriptor::parse_json_with_registry(
            RESOURCE,
            "assets/player.png.hycel.json",
            &resource_registry(),
        )
        .unwrap()
    }

    fn resource_registry() -> ResourceRegistry {
        let mut registry = ResourceRegistry::default();
        registry
            .register("texture", ["mipmaps", "quality"])
            .unwrap();
        registry
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
