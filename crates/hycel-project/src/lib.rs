//! Versioned project manifest and scene/resource schema parsing.
//!
//! Parsers are bounded, reject unknown fields, and return stable diagnostic
//! codes for user-authored data. They do not mutate the project or touch the
//! filesystem; path resolution and transactional migration belong to higher
//! layers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use serde_json::Value;

mod migration;
pub use migration::{
    MigrationReceipt, migrate_scene_file, migrate_scene_json, restore_scene_backup,
};

/// Maximum manifest file size in bytes.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Maximum scene or resource descriptor size in bytes.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum entity count in one scene.
pub const MAX_ENTITIES_PER_SCENE: usize = 100_000;
/// Maximum components on one entity.
pub const MAX_COMPONENTS_PER_ENTITY: usize = 256;

/// A stable, machine-readable validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Stable diagnostic code, suitable for tools and tests.
    pub code: &'static str,
    /// Project-relative file path, if known.
    pub file: Option<String>,
    /// TOML key path or JSONPath-like location.
    pub path: String,
    /// Human-readable explanation; wording may improve between releases.
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn new(
        code: &'static str,
        file: Option<&str>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            file: file.map(str::to_owned),
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{file}:{}: {} ({})", self.path, self.message, self.code)
        } else {
            write!(f, "{}: {} ({})", self.path, self.message, self.code)
        }
    }
}

impl std::error::Error for Diagnostic {}

/// Version-1 project manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    format_version: u32,
    project: ProjectIdentity,
    engine: EngineCompatibility,
    build: BuildSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectIdentity {
    id: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineCompatibility {
    min_version: String,
    max_version_exclusive: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildSettings {
    default_profile: String,
    profiles: BTreeMap<String, BuildProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildProfile {
    optimization: u8,
    debug_info: bool,
}

impl ProjectManifest {
    /// Parses and validates a UTF-8 TOML project manifest.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic for oversized, malformed, unsupported, or
    /// semantically invalid manifest content.
    pub fn parse_toml(input: &str) -> Result<Self, Diagnostic> {
        if input.len() > MAX_MANIFEST_BYTES {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-001",
                Some("hycel.toml"),
                "$",
                format!("manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit"),
            ));
        }
        let manifest: Self = toml::from_str(input).map_err(|error| {
            Diagnostic::new(
                "HYCEL-MANIFEST-002",
                Some("hycel.toml"),
                "$",
                format!("invalid or unsupported manifest fields: {error}"),
            )
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), Diagnostic> {
        if self.format_version != 1 {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-003",
                Some("hycel.toml"),
                "format_version",
                "unsupported manifest schema version",
            ));
        }
        validate_uuid(&self.project.id).map_err(|message| {
            Diagnostic::new(
                "HYCEL-MANIFEST-004",
                Some("hycel.toml"),
                "project.id",
                message,
            )
        })?;
        if self.project.name.trim().is_empty() || self.project.name.len() > 256 {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-005",
                Some("hycel.toml"),
                "project.name",
                "project name must contain 1–256 non-whitespace UTF-8 bytes",
            ));
        }
        let min = Version::parse(&self.engine.min_version).map_err(|error| {
            Diagnostic::new(
                "HYCEL-MANIFEST-006",
                Some("hycel.toml"),
                "engine.min_version",
                format!("invalid semantic version: {error}"),
            )
        })?;
        let max = Version::parse(&self.engine.max_version_exclusive).map_err(|error| {
            Diagnostic::new(
                "HYCEL-MANIFEST-007",
                Some("hycel.toml"),
                "engine.max_version_exclusive",
                format!("invalid semantic version: {error}"),
            )
        })?;
        if min >= max {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-008",
                Some("hycel.toml"),
                "engine",
                "minimum engine version must be lower than the exclusive maximum",
            ));
        }
        if !valid_profile_name(&self.build.default_profile) {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-009",
                Some("hycel.toml"),
                "build.default_profile",
                "profile name must start with a lowercase ASCII letter and contain only lowercase letters, digits, or hyphens",
            ));
        }
        if !self
            .build
            .profiles
            .contains_key(&self.build.default_profile)
        {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-010",
                Some("hycel.toml"),
                "build.default_profile",
                "default profile does not exist in build.profiles",
            ));
        }
        if self.build.profiles.is_empty() {
            return Err(Diagnostic::new(
                "HYCEL-MANIFEST-011",
                Some("hycel.toml"),
                "build.profiles",
                "at least one build profile is required",
            ));
        }
        for (name, profile) in &self.build.profiles {
            if !valid_profile_name(name) {
                return Err(Diagnostic::new(
                    "HYCEL-MANIFEST-012",
                    Some("hycel.toml"),
                    format!("build.profiles.{name}"),
                    "profile name must start with a lowercase ASCII letter and contain only lowercase letters, digits, or hyphens",
                ));
            }
            if profile.optimization > 3 {
                return Err(Diagnostic::new(
                    "HYCEL-MANIFEST-013",
                    Some("hycel.toml"),
                    format!("build.profiles.{name}.optimization"),
                    "optimization must be in 0..=3",
                ));
            }
        }
        Ok(())
    }

    /// Manifest schema version.
    #[must_use]
    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Stable project UUID string.
    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project.id
    }

    /// Display name.
    #[must_use]
    pub fn project_name(&self) -> &str {
        &self.project.name
    }

    /// Named build profiles, iterated in lexical order.
    pub fn build_profiles(&self) -> impl Iterator<Item = (&str, u8, bool)> {
        self.build
            .profiles
            .iter()
            .map(|(name, profile)| (name.as_str(), profile.optimization, profile.debug_info))
    }

    /// Name of the default build profile.
    #[must_use]
    pub fn default_profile(&self) -> &str {
        &self.build.default_profile
    }
}

/// Strict version-1 resource descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDescriptor {
    schema_version: u32,
    id: String,
    kind: String,
    source: String,
    import: BTreeMap<String, Value>,
}

impl ResourceDescriptor {
    /// Parses a descriptor with no registered resource kinds.
    ///
    /// # Errors
    ///
    /// Returns diagnostics for malformed or unregistered resource data.
    pub fn parse_json(input: &[u8], file: &str) -> Result<Self, Vec<Diagnostic>> {
        Self::parse_json_with_registry(input, file, &ResourceRegistry::default())
    }

    /// Parses a bounded resource descriptor using registered importer schemas.
    ///
    /// # Errors
    ///
    /// Returns stable diagnostics for malformed, oversized, unsupported, or
    /// semantically invalid resource data.
    pub fn parse_json_with_registry(
        input: &[u8],
        file: &str,
        registry: &ResourceRegistry,
    ) -> Result<Self, Vec<Diagnostic>> {
        if input.len() > MAX_DOCUMENT_BYTES {
            return Err(vec![Diagnostic::new(
                "HYCEL-DOCUMENT-001",
                Some(file),
                "$",
                format!("document exceeds the {MAX_DOCUMENT_BYTES}-byte limit"),
            )]);
        }
        let resource: Self = serde_json::from_slice(input).map_err(|error| {
            vec![Diagnostic::new(
                "HYCEL-RESOURCE-001",
                Some(file),
                "$",
                format!(
                    "invalid JSON or unknown/duplicate field at line {}, column {}: {error}",
                    error.line(),
                    error.column()
                ),
            )]
        })?;
        let mut diagnostics = Vec::new();
        if resource.schema_version != 1 {
            diagnostics.push(Diagnostic::new(
                "HYCEL-RESOURCE-002",
                Some(file),
                "$.schema_version",
                "unsupported resource schema version",
            ));
        }
        if let Err(message) = validate_uuid(&resource.id) {
            diagnostics.push(Diagnostic::new(
                "HYCEL-RESOURCE-003",
                Some(file),
                "$.id",
                message,
            ));
        }
        if !valid_component_type(&resource.kind) {
            diagnostics.push(Diagnostic::new(
                "HYCEL-RESOURCE-004",
                Some(file),
                "$.kind",
                "resource kind must be a lowercase identifier",
            ));
        }
        match registry.definitions.get(&resource.kind) {
            None => diagnostics.push(Diagnostic::new(
                "HYCEL-RESOURCE-006",
                Some(file),
                "$.kind",
                "resource kind is not registered",
            )),
            Some(allowed_settings) => {
                if let Some(setting) = resource
                    .import
                    .keys()
                    .find(|setting| !allowed_settings.contains(*setting))
                {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-RESOURCE-007",
                        Some(file),
                        format!("$.import.{setting}"),
                        "unknown importer setting",
                    ));
                }
            }
        }
        if let Err(mut diagnostic) = validate_relative_project_path(&resource.source) {
            diagnostic.code = "HYCEL-RESOURCE-005";
            diagnostic.file = Some(file.to_owned());
            "$.source".clone_into(&mut diagnostic.path);
            diagnostics.push(diagnostic);
        }
        if diagnostics.is_empty() {
            Ok(resource)
        } else {
            Err(diagnostics)
        }
    }

    /// Stable resource UUID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Importer kind identifier.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Project-relative authored source path.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Import settings in lexical key order.
    #[must_use]
    pub fn import_settings(&self) -> &BTreeMap<String, Value> {
        &self.import
    }
}

/// Registry of resource importer kinds and accepted settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceRegistry {
    definitions: BTreeMap<String, BTreeSet<String>>,
}

impl ResourceRegistry {
    /// Registers an importer kind and its allowed setting keys.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic for invalid identifiers, setting keys, or duplicate
    /// kind registration.
    pub fn register(
        &mut self,
        kind: &str,
        settings: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), Diagnostic> {
        if !valid_component_type(kind) {
            return Err(Diagnostic::new(
                "HYCEL-RESOURCE-REGISTRY-001",
                None,
                kind,
                "invalid resource kind identifier",
            ));
        }
        let settings = settings
            .into_iter()
            .map(Into::into)
            .collect::<BTreeSet<_>>();
        if settings.iter().any(|setting| {
            setting.is_empty()
                || !setting
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        }) {
            return Err(Diagnostic::new(
                "HYCEL-RESOURCE-REGISTRY-002",
                None,
                kind,
                "setting names must be lowercase ASCII identifiers",
            ));
        }
        if self.definitions.insert(kind.to_owned(), settings).is_some() {
            return Err(Diagnostic::new(
                "HYCEL-RESOURCE-REGISTRY-003",
                None,
                kind,
                "resource kind is already registered",
            ));
        }
        Ok(())
    }
}

/// Validates project-wide stable ID uniqueness and registered references.
///
/// Scene/resource documents should first be parsed with their strict parsers.
/// Each `(file, document)` pair retains source context in any returned issue.
///
/// # Errors
///
/// Returns diagnostics for duplicate project IDs, non-string references, and
/// dangling entity/resource references declared in the component registry.
pub fn validate_project_documents(
    scenes: &[(String, SceneDocument)],
    resources: &[(String, ResourceDescriptor)],
    registry: &ComponentRegistry,
) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    let mut ids = BTreeMap::<String, (String, String)>::new();
    let mut entity_ids = BTreeSet::new();
    let mut resource_ids = BTreeSet::new();
    for (file, scene) in scenes {
        insert_project_id(&mut ids, &mut diagnostics, &scene.id, file, "$.id");
        for (index, entity) in scene.entities.iter().enumerate() {
            insert_project_id(
                &mut ids,
                &mut diagnostics,
                &entity.id,
                file,
                &format!("$.entities[{index}].id"),
            );
            entity_ids.insert(entity.id.as_str());
        }
    }
    for (file, resource) in resources {
        insert_project_id(&mut ids, &mut diagnostics, &resource.id, file, "$.id");
        resource_ids.insert(resource.id.as_str());
    }
    for (file, scene) in scenes {
        for (entity_index, entity) in scene.entities.iter().enumerate() {
            for (component_index, component) in entity.components.iter().enumerate() {
                let Some(definition) = registry.definitions.get(&component.component_type) else {
                    continue;
                };
                for (field, targets, kind) in [
                    (&definition.entity_reference_fields, &entity_ids, "entity"),
                    (
                        &definition.resource_reference_fields,
                        &resource_ids,
                        "resource",
                    ),
                ] {
                    for field in field {
                        let path = format!(
                            "$.entities[{entity_index}].components[{component_index}].data.{field}"
                        );
                        match component.data.get(field).and_then(Value::as_str) {
                            Some(target) if targets.contains(target) => {}
                            Some(_) => diagnostics.push(Diagnostic::new(
                                "HYCEL-PROJECT-002",
                                Some(file),
                                path,
                                format!("referenced {kind} UUID does not exist in this project"),
                            )),
                            None => diagnostics.push(Diagnostic::new(
                                "HYCEL-PROJECT-003",
                                Some(file),
                                path,
                                format!("reference to {kind} must be a UUID string"),
                            )),
                        }
                    }
                }
            }
        }
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

fn insert_project_id(
    ids: &mut BTreeMap<String, (String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
    id: &str,
    file: &str,
    path: &str,
) {
    if let Some((first_file, first_path)) = ids.get(id) {
        diagnostics.push(Diagnostic::new(
            "HYCEL-PROJECT-001",
            Some(file),
            path,
            format!("UUID duplicates {first_file}:{first_path}"),
        ));
    } else {
        ids.insert(id.to_owned(), (file.to_owned(), path.to_owned()));
    }
}

/// Registry of component schema versions and accepted payload field names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComponentRegistry {
    definitions: BTreeMap<String, ComponentDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComponentDefinition {
    schema_version: u32,
    repeatable: bool,
    fields: BTreeSet<String>,
    entity_reference_fields: BTreeSet<String>,
    resource_reference_fields: BTreeSet<String>,
}

impl ComponentRegistry {
    /// Registers one component schema and its allowed payload keys.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic for an invalid type/version/field identifier or a
    /// duplicate type registration.
    pub fn register(
        &mut self,
        component_type: &str,
        schema_version: u32,
        repeatable: bool,
        fields: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), Diagnostic> {
        if !valid_component_type(component_type) || schema_version == 0 {
            return Err(Diagnostic::new(
                "HYCEL-REGISTRY-001",
                None,
                component_type,
                "invalid component type or schema version",
            ));
        }
        let fields = fields.into_iter().map(Into::into).collect::<BTreeSet<_>>();
        if fields.iter().any(|field| {
            field.is_empty()
                || !field
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        }) {
            return Err(Diagnostic::new(
                "HYCEL-REGISTRY-002",
                None,
                component_type,
                "component payload field names must be lowercase ASCII identifiers",
            ));
        }
        if self.definitions.contains_key(component_type) {
            return Err(Diagnostic::new(
                "HYCEL-REGISTRY-003",
                None,
                component_type,
                "component type is already registered",
            ));
        }
        self.definitions.insert(
            component_type.to_owned(),
            ComponentDefinition {
                schema_version,
                repeatable,
                fields,
                entity_reference_fields: BTreeSet::new(),
                resource_reference_fields: BTreeSet::new(),
            },
        );
        Ok(())
    }

    /// Declares an accepted payload field as a persistent entity UUID reference.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if the component or field is not registered.
    pub fn mark_entity_reference(
        &mut self,
        component_type: &str,
        field: &str,
    ) -> Result<(), Diagnostic> {
        self.mark_reference(component_type, field, true)
    }

    /// Declares an accepted payload field as a persistent resource UUID reference.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if the component or field is not registered.
    pub fn mark_resource_reference(
        &mut self,
        component_type: &str,
        field: &str,
    ) -> Result<(), Diagnostic> {
        self.mark_reference(component_type, field, false)
    }

    fn mark_reference(
        &mut self,
        component_type: &str,
        field: &str,
        entity: bool,
    ) -> Result<(), Diagnostic> {
        let Some(definition) = self.definitions.get_mut(component_type) else {
            return Err(Diagnostic::new(
                "HYCEL-REGISTRY-004",
                None,
                component_type,
                "component type is not registered",
            ));
        };
        if !definition.fields.contains(field) {
            return Err(Diagnostic::new(
                "HYCEL-REGISTRY-005",
                None,
                field,
                "reference field is not in the component schema",
            ));
        }
        if entity {
            definition.entity_reference_fields.insert(field.to_owned());
        } else {
            definition
                .resource_reference_fields
                .insert(field.to_owned());
        }
        Ok(())
    }
}

/// Strict versioned scene document with already validated local references.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneDocument {
    schema_version: u32,
    id: String,
    name: String,
    entities: Vec<SceneEntity>,
}

/// Scene entity with persistent authoring identity and runtime-neutral data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneEntity {
    id: String,
    name: String,
    #[serde(default)]
    tags: Field<Vec<String>>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    transform: SceneTransform,
    #[serde(default)]
    components: Vec<ComponentRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Field<T> {
    #[default]
    Missing,
    Present(T),
}

impl<'de, T> Deserialize<'de> for Field<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneTransform {
    #[serde(default)]
    translation_milli: [i64; 2],
    #[serde(default)]
    rotation_units: u16,
    #[serde(default = "unit_scale")]
    scale_milli: [i64; 2],
}

impl Default for SceneTransform {
    fn default() -> Self {
        Self {
            translation_milli: [0, 0],
            rotation_units: 0,
            scale_milli: unit_scale(),
        }
    }
}

const fn unit_scale() -> [i64; 2] {
    [1_000, 1_000]
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRecord {
    #[serde(rename = "type")]
    component_type: String,
    schema_version: u32,
    data: BTreeMap<String, Value>,
}

impl SceneDocument {
    /// Parses strict JSON and validates IDs, values, component bounds, parents,
    /// and hierarchy cycles. `file` is retained in all returned diagnostics.
    /// The default registry accepts no custom component types.
    ///
    /// # Errors
    ///
    /// Returns stable diagnostics for oversized, malformed, unsupported, or
    /// semantically invalid scene content.
    pub fn parse_json(input: &[u8], file: &str) -> Result<Self, Vec<Diagnostic>> {
        Self::parse_json_with_registry(input, file, &ComponentRegistry::default())
    }

    /// Parses a scene using the caller's registered component schemas.
    ///
    /// # Errors
    ///
    /// Returns stable diagnostics for malformed documents and unregistered
    /// component types, versions, or payload fields.
    pub fn parse_json_with_registry(
        input: &[u8],
        file: &str,
        registry: &ComponentRegistry,
    ) -> Result<Self, Vec<Diagnostic>> {
        if input.len() > MAX_DOCUMENT_BYTES {
            return Err(vec![Diagnostic::new(
                "HYCEL-DOCUMENT-001",
                Some(file),
                "$",
                format!("document exceeds the {MAX_DOCUMENT_BYTES}-byte limit"),
            )]);
        }
        let scene: Self = serde_json::from_slice(input).map_err(|error| {
            vec![Diagnostic::new(
                "HYCEL-SCENE-001",
                Some(file),
                "$",
                format!(
                    "invalid JSON or unknown/duplicate field at line {}, column {}: {error}",
                    error.line(),
                    error.column()
                ),
            )]
        })?;
        scene.validate(file, registry)?;
        Ok(scene)
    }

    #[allow(clippy::too_many_lines)]
    fn validate(&self, file: &str, registry: &ComponentRegistry) -> Result<(), Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();
        if !matches!(self.schema_version, 1 | 2) {
            diagnostics.push(Diagnostic::new(
                "HYCEL-SCENE-002",
                Some(file),
                "$.schema_version",
                "unsupported scene schema version",
            ));
        }
        if let Err(message) = validate_uuid(&self.id) {
            diagnostics.push(Diagnostic::new(
                "HYCEL-SCENE-003",
                Some(file),
                "$.id",
                message,
            ));
        }
        validate_name(&self.name, "$.name", file, &mut diagnostics);
        if self.entities.len() > MAX_ENTITIES_PER_SCENE {
            diagnostics.push(Diagnostic::new(
                "HYCEL-SCENE-004",
                Some(file),
                "$.entities",
                format!("scene exceeds the {MAX_ENTITIES_PER_SCENE}-entity limit"),
            ));
            return Err(diagnostics);
        }

        let mut ids = BTreeSet::new();
        for (index, entity) in self.entities.iter().enumerate() {
            let base = format!("$.entities[{index}]");
            if let Err(message) = validate_uuid(&entity.id) {
                diagnostics.push(Diagnostic::new(
                    "HYCEL-SCENE-005",
                    Some(file),
                    format!("{base}.id"),
                    message,
                ));
            }
            if !ids.insert(entity.id.as_str()) {
                diagnostics.push(Diagnostic::new(
                    "HYCEL-SCENE-006",
                    Some(file),
                    format!("{base}.id"),
                    "duplicate entity UUID",
                ));
            }
            validate_name(
                &entity.name,
                &format!("{base}.name"),
                file,
                &mut diagnostics,
            );
            match (self.schema_version, &entity.tags) {
                (1, Field::Present(_)) => diagnostics.push(Diagnostic::new(
                    "HYCEL-SCENE-018",
                    Some(file),
                    format!("{base}.tags"),
                    "tags are only supported in scene schema version 2",
                )),
                (2, Field::Missing) => diagnostics.push(Diagnostic::new(
                    "HYCEL-SCENE-019",
                    Some(file),
                    format!("{base}.tags"),
                    "tags is required in scene schema version 2",
                )),
                (_, Field::Present(tags)) => {
                    let mut seen = BTreeSet::new();
                    for (tag_index, tag) in tags.iter().enumerate() {
                        if !valid_tag(tag) {
                            diagnostics.push(Diagnostic::new(
                                "HYCEL-SCENE-020",
                                Some(file),
                                format!("{base}.tags[{tag_index}]"),
                                "tag must be 1–64 lowercase ASCII letters, digits, '.', '_' or '-', beginning with a letter or digit",
                            ));
                        }
                        if !seen.insert(tag.as_str()) {
                            diagnostics.push(Diagnostic::new(
                                "HYCEL-SCENE-021",
                                Some(file),
                                format!("{base}.tags[{tag_index}]"),
                                "duplicate entity tag",
                            ));
                        }
                    }
                }
                (_, Field::Missing) => {}
            }
            if entity.components.len() > MAX_COMPONENTS_PER_ENTITY {
                diagnostics.push(Diagnostic::new(
                    "HYCEL-SCENE-007",
                    Some(file),
                    format!("{base}.components"),
                    format!("entity exceeds the {MAX_COMPONENTS_PER_ENTITY}-component limit"),
                ));
            }
            let mut component_types = BTreeSet::new();
            for (component_index, component) in entity.components.iter().enumerate() {
                let component_path = format!("{base}.components[{component_index}]");
                if !valid_component_type(&component.component_type) {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-SCENE-008",
                        Some(file),
                        format!("{component_path}.type"),
                        "component type must be a lowercase dotted identifier",
                    ));
                }
                if component.schema_version == 0 {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-SCENE-009",
                        Some(file),
                        format!("{component_path}.schema_version"),
                        "component schema version must be positive",
                    ));
                }
                match registry.definitions.get(&component.component_type) {
                    None => diagnostics.push(Diagnostic::new(
                        "HYCEL-SCENE-015",
                        Some(file),
                        format!("{component_path}.type"),
                        "component type is not registered",
                    )),
                    Some(definition) => {
                        if component.schema_version != definition.schema_version {
                            diagnostics.push(Diagnostic::new(
                                "HYCEL-SCENE-016",
                                Some(file),
                                format!("{component_path}.schema_version"),
                                "component schema version is not registered",
                            ));
                        }
                        if let Some(field) = component
                            .data
                            .keys()
                            .find(|field| !definition.fields.contains(*field))
                        {
                            diagnostics.push(Diagnostic::new(
                                "HYCEL-SCENE-017",
                                Some(file),
                                format!("{component_path}.data.{field}"),
                                "unknown component payload field",
                            ));
                        }
                        if !definition.repeatable
                            && !component_types.insert(component.component_type.as_str())
                        {
                            diagnostics.push(Diagnostic::new(
                                "HYCEL-SCENE-010",
                                Some(file),
                                format!("{component_path}.type"),
                                "component type is not repeatable on one entity",
                            ));
                        }
                    }
                }
            }
        }

        let entity_ids = self
            .entities
            .iter()
            .map(|entity| entity.id.as_str())
            .collect::<BTreeSet<_>>();
        for (index, entity) in self.entities.iter().enumerate() {
            if let Some(parent) = &entity.parent {
                if validate_uuid(parent).is_err() || !entity_ids.contains(parent.as_str()) {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-SCENE-011",
                        Some(file),
                        format!("$.entities[{index}].parent"),
                        "parent must reference an entity UUID in this scene",
                    ));
                }
                if parent == &entity.id {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-SCENE-012",
                        Some(file),
                        format!("$.entities[{index}].parent"),
                        "entity cannot be its own parent",
                    ));
                }
            }
        }
        self.validate_parent_cycles(file, &mut diagnostics);
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(diagnostics)
        }
    }

    fn validate_parent_cycles(&self, file: &str, diagnostics: &mut Vec<Diagnostic>) {
        let parents = self
            .entities
            .iter()
            .map(|entity| (entity.id.as_str(), entity.parent.as_deref()))
            .collect::<BTreeMap<_, _>>();
        let mut complete = BTreeSet::new();
        for entity in &self.entities {
            let mut chain = BTreeSet::new();
            let mut current = Some(entity.id.as_str());
            while let Some(id) = current {
                if complete.contains(id) {
                    break;
                }
                if !chain.insert(id) {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-SCENE-013",
                        Some(file),
                        "$.entities",
                        format!("parent hierarchy contains a cycle involving entity {id}"),
                    ));
                    break;
                }
                current = parents.get(id).copied().flatten();
            }
            complete.extend(chain);
            if !diagnostics.is_empty() {
                break;
            }
        }
    }

    /// Scene schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Scene UUID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Scene display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Entities in authored array order.
    #[must_use]
    pub fn entities(&self) -> &[SceneEntity] {
        &self.entities
    }
}

impl SceneEntity {
    /// Persistent scene UUID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Entity tags in authored array order.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        match &self.tags {
            Field::Missing => &[],
            Field::Present(tags) => tags,
        }
    }

    /// Parent UUID, if any.
    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    /// Transform values, including documented identity defaults.
    #[must_use]
    pub const fn transform(&self) -> SceneTransform {
        self.transform
    }

    /// Component records in authored order.
    #[must_use]
    pub fn components(&self) -> &[ComponentRecord] {
        &self.components
    }
}

impl SceneTransform {
    /// Translation in milli-world-units.
    #[must_use]
    pub const fn translation_milli(self) -> [i64; 2] {
        self.translation_milli
    }

    /// Clockwise angle in canonical turn units.
    #[must_use]
    pub const fn rotation_units(self) -> u16 {
        self.rotation_units
    }

    /// Non-uniform scale in milli-world-units.
    #[must_use]
    pub const fn scale_milli(self) -> [i64; 2] {
        self.scale_milli
    }
}

impl ComponentRecord {
    /// Registered component type identifier.
    #[must_use]
    pub fn component_type(&self) -> &str {
        &self.component_type
    }

    /// Component payload object.
    #[must_use]
    pub fn data(&self) -> &BTreeMap<String, Value> {
        &self.data
    }
}

fn read_limited_file(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut limited = file.take(limit as u64 + 1);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    limited.read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds the configured size limit",
        ));
    }
    Ok(bytes)
}

/// Validates an existing project root, manifest, and fixed content directories.
///
/// The function is read-only. Existing generated directories are also checked
/// for symlink escapes before callers may write into them.
///
/// # Errors
///
/// Returns diagnostics for an unavailable root/manifest, invalid manifest
/// content, missing content directories, or paths resolving outside the root.
#[allow(clippy::too_many_lines)]
pub fn validate_project_root(project_root: &Path) -> Result<ProjectManifest, Vec<Diagnostic>> {
    let canonical_root = project_root.canonicalize().map_err(|error| {
        vec![Diagnostic::new(
            "HYCEL-PROJECT-004",
            None,
            "project_root",
            format!("cannot resolve project root: {error}"),
        )]
    })?;
    if !canonical_root.is_dir() {
        return Err(vec![Diagnostic::new(
            "HYCEL-PROJECT-005",
            None,
            "project_root",
            "project root is not a directory",
        )]);
    }
    let mut diagnostics = Vec::new();
    for relative in ["src", "assets", "scenes"] {
        match resolve_existing_project_path(&canonical_root, relative) {
            Ok(path) if path.is_dir() => {}
            Ok(_) => diagnostics.push(Diagnostic::new(
                "HYCEL-PROJECT-006",
                Some(relative),
                "$",
                "required content root is not a directory",
            )),
            Err(mut diagnostic) => {
                diagnostic.code = "HYCEL-PROJECT-007";
                diagnostics.push(diagnostic);
            }
        }
    }
    for generated in ["build", ".hycel"] {
        let path = canonical_root.join(generated);
        if std::fs::symlink_metadata(&path).is_ok() {
            match resolve_existing_project_path(&canonical_root, generated) {
                Ok(_) => {}
                Err(mut diagnostic) => {
                    diagnostic.code = "HYCEL-PROJECT-008";
                    diagnostics.push(diagnostic);
                }
            }
        }
    }
    let manifest_path = match resolve_existing_project_path(&canonical_root, "hycel.toml") {
        Ok(path) if path.is_file() => Some(path),
        Ok(_) => {
            diagnostics.push(Diagnostic::new(
                "HYCEL-PROJECT-009",
                Some("hycel.toml"),
                "$",
                "project manifest is not a regular file",
            ));
            None
        }
        Err(mut diagnostic) => {
            diagnostic.code = "HYCEL-PROJECT-010";
            diagnostics.push(diagnostic);
            None
        }
    };
    let manifest = if let Some(path) = manifest_path {
        match read_limited_file(&path, MAX_MANIFEST_BYTES) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(contents) => match ProjectManifest::parse_toml(&contents) {
                    Ok(manifest) => Some(manifest),
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        None
                    }
                },
                Err(error) => {
                    diagnostics.push(Diagnostic::new(
                        "HYCEL-MANIFEST-002",
                        Some("hycel.toml"),
                        "$",
                        format!("manifest is not valid UTF-8: {error}"),
                    ));
                    None
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                diagnostics.push(Diagnostic::new(
                    "HYCEL-MANIFEST-001",
                    Some("hycel.toml"),
                    "$",
                    format!("manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit"),
                ));
                None
            }
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    "HYCEL-PROJECT-011",
                    Some("hycel.toml"),
                    "$",
                    format!("cannot read project manifest: {error}"),
                ));
                None
            }
        }
    } else {
        None
    };
    match (manifest, diagnostics.is_empty()) {
        (Some(manifest), true) => Ok(manifest),
        (_, false) => Err(diagnostics),
        (None, true) => Err(vec![Diagnostic::new(
            "HYCEL-PROJECT-012",
            Some("hycel.toml"),
            "$",
            "project validation completed without a parsed manifest",
        )]),
    }
}

/// Checks that a project-relative path is lexically confined to its root.
///
/// This is a lexical check only. File-opening code must additionally resolve
/// symlinks against a canonical project root before reading or writing.
///
/// # Errors
///
/// Returns a diagnostic when the path is empty, absolute, contains traversal,
/// or uses a non-portable separator.
pub fn validate_relative_project_path(path: &str) -> Result<(), Diagnostic> {
    let invalid = path.is_empty()
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'));
    if invalid {
        return Err(Diagnostic::new(
            "HYCEL-PATH-001",
            None,
            "path",
            "path must be a non-empty UTF-8 project-relative path without traversal, absolute prefixes, or backslashes",
        ));
    }
    Ok(())
}

/// Resolves an existing project-relative file or directory and rejects symlink
/// escapes from the canonical project root.
///
/// # Errors
///
/// Returns a diagnostic for an invalid relative path, an unavailable path, or
/// a canonical target outside the project root.
pub fn resolve_existing_project_path(
    project_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, Diagnostic> {
    validate_relative_project_path(relative_path)?;
    let canonical_root = project_root.canonicalize().map_err(|error| {
        Diagnostic::new(
            "HYCEL-PATH-002",
            None,
            "project_root",
            format!("cannot resolve project root: {error}"),
        )
    })?;
    let candidate = canonical_root
        .join(relative_path)
        .canonicalize()
        .map_err(|error| {
            Diagnostic::new(
                "HYCEL-PATH-003",
                Some(relative_path),
                "$",
                format!("project path cannot be resolved: {error}"),
            )
        })?;
    if !candidate.starts_with(&canonical_root) {
        return Err(Diagnostic::new(
            "HYCEL-PATH-004",
            Some(relative_path),
            "$",
            "resolved path escapes the canonical project root",
        ));
    }
    Ok(candidate)
}

fn validate_uuid(value: &str) -> Result<(), &'static str> {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || [8, 13, 18, 23]
            .into_iter()
            .any(|index| bytes[index] != b'-')
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| ![8, 13, 18, 23].contains(&index) && !byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err("ID must be a canonical lowercase hyphenated UUID");
    }
    Ok(())
}

fn valid_profile_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_component_type(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn valid_tag(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn validate_name(value: &str, path: &str, file: &str, diagnostics: &mut Vec<Diagnostic>) {
    if value.trim().is_empty() || value.len() > 256 {
        diagnostics.push(Diagnostic::new(
            "HYCEL-SCENE-014",
            Some(file),
            path,
            "name must contain 1–256 non-whitespace UTF-8 bytes",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComponentRegistry, MAX_DOCUMENT_BYTES, MAX_MANIFEST_BYTES, ProjectManifest,
        ResourceDescriptor, ResourceRegistry, SceneDocument, validate_project_documents,
        validate_relative_project_path,
    };

    const MANIFEST: &str = include_str!("../../../examples/empty-project/hycel.toml");
    const SCENE: &[u8] = include_bytes!("../../../examples/empty-project/scenes/first-room.json");

    #[test]
    fn example_manifest_and_scene_parse() {
        let manifest = ProjectManifest::parse_toml(MANIFEST).unwrap();
        assert_eq!(manifest.project_name(), "My Game");
        assert_eq!(manifest.default_profile(), "development");
        let scene = SceneDocument::parse_json(SCENE, "scenes/first-room.json").unwrap();
        assert_eq!(scene.name(), "First Room");
        assert_eq!(scene.schema_version(), 2);
        assert_eq!(scene.entities().len(), 1);
        assert_eq!(scene.entities()[0].tags()[0], "player");
    }

    #[test]
    fn manifest_rejects_unknown_fields_bad_versions_and_missing_default_profile() {
        let unknown = format!("{MANIFEST}\ntelemetry = true\n");
        assert_eq!(
            ProjectManifest::parse_toml(&unknown).unwrap_err().code,
            "HYCEL-MANIFEST-002"
        );
        let bad_version = MANIFEST.replace("format_version = 1", "format_version = 2");
        assert_eq!(
            ProjectManifest::parse_toml(&bad_version).unwrap_err().code,
            "HYCEL-MANIFEST-003"
        );
        let bad_default = MANIFEST.replace(
            "default_profile = \"development\"",
            "default_profile = \"missing\"",
        );
        assert_eq!(
            ProjectManifest::parse_toml(&bad_default).unwrap_err().code,
            "HYCEL-MANIFEST-010"
        );
    }

    #[test]
    fn manifest_size_limit_is_checked_before_parsing() {
        let oversized = " ".repeat(MAX_MANIFEST_BYTES + 1);
        assert_eq!(
            ProjectManifest::parse_toml(&oversized).unwrap_err().code,
            "HYCEL-MANIFEST-001"
        );
    }

    #[test]
    fn scene_rejects_unknown_and_duplicate_fields() {
        let unknown = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[],"unknown":1}"#;
        let error = SceneDocument::parse_json(unknown, "scenes/room.json").unwrap_err();
        assert_eq!(error[0].code, "HYCEL-SCENE-001");
        assert_eq!(error[0].file.as_deref(), Some("scenes/room.json"));
        let duplicate = br#"{"schema_version":1,"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[]}"#;
        assert_eq!(
            SceneDocument::parse_json(duplicate, "room.json").unwrap_err()[0].code,
            "HYCEL-SCENE-001"
        );
    }

    #[test]
    fn scene_schema_two_requires_unique_valid_entity_tags() {
        let valid = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","tags":["enemy","flying"]}]}"#;
        let scene = SceneDocument::parse_json(valid, "room.json").unwrap();
        assert_eq!(
            scene.entities()[0]
                .tags()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["enemy", "flying"]
        );

        let duplicate = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","tags":["enemy","enemy"]}]}"#;
        assert!(
            SceneDocument::parse_json(duplicate, "room.json")
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-021")
        );

        let missing_tags = br#"{"schema_version":2,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A"}]}"#;
        assert!(
            SceneDocument::parse_json(missing_tags, "room.json")
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-019")
        );

        let v1_tags = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","tags":[] }]}"#;
        assert!(
            SceneDocument::parse_json(v1_tags, "room.json")
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-018")
        );
    }

    #[test]
    fn scene_validates_duplicate_ids_missing_parents_and_cycles() {
        let duplicate = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A"},{"id":"20000000-0000-4000-8000-000000000001","name":"B"}]}"#;
        assert!(
            SceneDocument::parse_json(duplicate, "room.json")
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-006")
        );
        let missing_parent = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","parent":"20000000-0000-4000-8000-000000000002"}]}"#;
        assert!(
            SceneDocument::parse_json(missing_parent, "room.json")
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-011")
        );
        let cycle = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","parent":"20000000-0000-4000-8000-000000000002"},{"id":"20000000-0000-4000-8000-000000000002","name":"B","parent":"20000000-0000-4000-8000-000000000001"}]}"#;
        assert!(
            SceneDocument::parse_json(cycle, "room.json")
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-013")
        );
    }

    #[test]
    fn scene_defaults_transform_and_rejects_bad_component_values() {
        let scene = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A"}]}"#;
        let parsed = SceneDocument::parse_json(scene, "room.json").unwrap();
        assert_eq!(parsed.entities()[0].components().len(), 0);
        let bad_component = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","components":[{"type":"bad Type","schema_version":0,"data":{}}]}]}"#;
        let diagnostics = SceneDocument::parse_json(bad_component, "room.json").unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-008")
        );
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-009")
        );
    }

    #[test]
    fn document_size_limit_is_checked_before_json_parse() {
        let oversized = vec![b' '; MAX_DOCUMENT_BYTES + 1];
        assert_eq!(
            SceneDocument::parse_json(&oversized, "room.json").unwrap_err()[0].code,
            "HYCEL-DOCUMENT-001"
        );
    }

    #[test]
    fn component_registry_rejects_unknown_versions_and_payload_fields() {
        let mut registry = ComponentRegistry::default();
        registry
            .register(
                "hycel.platformer.body",
                1,
                false,
                ["speed", "jump_strength"],
            )
            .unwrap();
        let valid = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","components":[{"type":"hycel.platformer.body","schema_version":1,"data":{"speed":5}}]}]}"#;
        assert!(SceneDocument::parse_json_with_registry(valid, "room.json", &registry).is_ok());
        let unknown_field = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","components":[{"type":"hycel.platformer.body","schema_version":1,"data":{"speeed":5}}]}]}"#;
        assert!(
            SceneDocument::parse_json_with_registry(unknown_field, "room.json", &registry)
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-017")
        );
        let unknown_type = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"A","components":[{"type":"hycel.unknown","schema_version":1,"data":{}}]}]}"#;
        assert!(
            SceneDocument::parse_json_with_registry(unknown_type, "room.json", &registry)
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-SCENE-015")
        );
    }

    #[test]
    fn project_validator_checks_global_ids_and_declared_resource_references() {
        let mut registry = ComponentRegistry::default();
        registry
            .register("hycel.sprite", 1, false, ["texture"])
            .unwrap();
        registry
            .mark_resource_reference("hycel.sprite", "texture")
            .unwrap();
        let scene_json = br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","name":"Room","entities":[{"id":"20000000-0000-4000-8000-000000000001","name":"Player","components":[{"type":"hycel.sprite","schema_version":1,"data":{"texture":"30000000-0000-4000-8000-000000000001"}}]}]}"#;
        let scene =
            SceneDocument::parse_json_with_registry(scene_json, "scenes/room.json", &registry)
                .unwrap();
        let mut resource_registry = ResourceRegistry::default();
        resource_registry
            .register("texture", std::iter::empty::<&str>())
            .unwrap();
        let resource_json = br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"texture","source":"sprites/player.png","import":{}}"#;
        let resource = ResourceDescriptor::parse_json_with_registry(
            resource_json,
            "assets/player.hycel.json",
            &resource_registry,
        )
        .unwrap();
        let scenes = vec![("scenes/room.json".to_owned(), scene.clone())];
        let resources = vec![("assets/player.hycel.json".to_owned(), resource)];
        assert!(validate_project_documents(&scenes, &resources, &registry).is_ok());
        assert!(
            validate_project_documents(&scenes, &[], &registry)
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-PROJECT-002")
        );
        let duplicate = ResourceDescriptor::parse_json_with_registry(
            br#"{"schema_version":1,"id":"10000000-0000-4000-8000-000000000001","kind":"texture","source":"sprites/duplicate.png","import":{}}"#,
            "assets/duplicate.hycel.json",
            &resource_registry,
        ).unwrap();
        assert!(
            validate_project_documents(
                &scenes,
                &[("assets/duplicate.hycel.json".to_owned(), duplicate)],
                &registry,
            )
            .unwrap_err()
            .iter()
            .any(|diagnostic| diagnostic.code == "HYCEL-PROJECT-001")
        );
    }

    #[test]
    fn resource_descriptor_rejects_unsafe_source_path() {
        let mut registry = ResourceRegistry::default();
        registry
            .register("texture", std::iter::empty::<&str>())
            .unwrap();
        let data = br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"texture","source":"../secret.png","import":{}}"#;
        assert_eq!(
            ResourceDescriptor::parse_json_with_registry(
                data,
                "assets/player.hycel.json",
                &registry
            )
            .unwrap_err()[0]
                .code,
            "HYCEL-RESOURCE-005"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_root_requires_fixed_directories_and_rejects_generated_symlink_escape() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(100);
        let base = std::env::temp_dir().join(format!(
            "hycel-project-root-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let project = base.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join("assets")).unwrap();
        std::fs::create_dir_all(project.join("scenes")).unwrap();
        std::fs::write(project.join("hycel.toml"), MANIFEST).unwrap();
        assert!(super::validate_project_root(&project).is_ok());
        let outside = base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, project.join(".hycel")).unwrap();
        assert!(
            super::validate_project_root(&project)
                .unwrap_err()
                .iter()
                .any(|diagnostic| diagnostic.code == "HYCEL-PROJECT-008")
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn resolved_symlink_may_not_escape_project_root() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "hycel-path-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let project = base.join("project");
        let outside = base.join("outside.txt");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, project.join("escape.txt")).unwrap();
        assert_eq!(
            super::resolve_existing_project_path(&project, "escape.txt")
                .unwrap_err()
                .code,
            "HYCEL-PATH-004"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn project_paths_reject_traversal_and_absolute_forms() {
        for invalid in [
            "",
            "../outside",
            "assets/../../secret",
            "/absolute/path",
            "C:\\outside",
            "assets\\file.png",
        ] {
            assert!(
                validate_relative_project_path(invalid).is_err(),
                "{invalid:?}"
            );
        }
        assert!(validate_relative_project_path("assets/sprites/player.png").is_ok());
    }
}
