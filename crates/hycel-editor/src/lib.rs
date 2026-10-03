//! Stateful authoring model shared by the native editor shell and round-trip tests.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hycel_assets::{ImportRecord, ReimportDecision, hash_source_file, plan_reimport};
use hycel_cli::{
    SceneEditPreview, SceneEditReceipt, apply_scene_edit, execute, preview_scene_edit,
};
use hycel_project::{
    MAX_DOCUMENT_BYTES, ResourceDescriptor, ResourceRegistry, SceneEditOperation,
    resolve_existing_project_path,
};
use serde::Serialize;
use serde_json::Value;

const MAX_SCENES: usize = 1024;
const MAX_RESOURCES: usize = 100_000;
const MAX_ENTITIES_PER_SCENE: usize = 4096;
const MAX_RESOURCE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMPORT_RECORDS_PER_RESOURCE: usize = 64;
const MAX_TEST_CASES: usize = 16;
const MAX_TEST_NAME_CHARS: usize = 128;
const MAX_TEST_MESSAGE_CHARS: usize = 4_096;
const MAX_TEST_REPORT_BYTES: usize = 64 * 1024;
const MAX_TEST_REPORT_ARTIFACTS: usize = 64;
const MAX_PREVIEW_DIRECTORY_ENTRIES: usize = 4_096;
const EDITOR_TEST_REPORT_SCHEMA_VERSION: u32 = 1;
static TEST_REPORT_EXPORT_LOCK: Mutex<()> = Mutex::new(());

/// Read-only view of one entity in the editor hierarchy/viewport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorEntity {
    /// Persistent stable entity UUID.
    pub id: String,
    /// Authored display name.
    pub name: String,
    /// Translation in milli-world-units.
    pub translation_milli: [i64; 2],
    /// Scale in milli-units.
    pub scale_milli: [i64; 2],
    /// Authored schema-2 entity tags.
    pub tags: Vec<String>,
    /// Registered component type identifiers.
    pub component_types: Vec<String>,
}

/// Read-only resource/import row from the shared project inspection service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorResource {
    /// Stable resource UUID.
    pub id: String,
    /// Resource category (for example, texture or animation).
    pub kind: String,
    /// Project-relative descriptor path.
    pub descriptor_file: String,
    /// Project-relative source asset path.
    pub source: String,
    /// Number of direct resource dependencies reported by the CLI.
    pub dependency_count: usize,
}

/// Cached import metadata and deterministic reimport decisions for one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorImportRecordStatus {
    /// Content fingerprint and cache key.
    pub fingerprint_sha256: String,
    /// Importer version captured by the record.
    pub importer_version: String,
    /// Input decision relative to this record's captured importer version; not a freshness check.
    pub decision_using_recorded_version: ReimportDecision,
    /// Whether the cache contains at least one derived file besides `record.json`.
    pub has_derived_output: bool,
}

/// Bounded, read-only inspection result for the selected resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorResourceImportStatus {
    /// SHA-256 of the current authored source.
    pub source_sha256: String,
    /// Validated cache records in fingerprint order.
    pub records: Vec<EditorImportRecordStatus>,
}

/// One named editor test result from the compiled-in reference runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EditorTestCase {
    /// Scenario identifier.
    pub name: String,
    /// Whether all assertions passed.
    pub passed: bool,
    /// Bounded human-readable outcome or failure detail.
    pub message: String,
}

/// Bounded test run report, suitable for rendering or saving as a diagnostic artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EditorTestReport {
    /// Individual named scenarios.
    pub tests: Vec<EditorTestCase>,
    /// Number of scenarios that passed.
    pub passed_count: usize,
    /// Number of scenarios that failed.
    pub failed_count: usize,
}

/// Read-only view of a validated project scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorScene {
    /// Project-relative scene file.
    pub relative_path: String,
    /// Persistent scene UUID.
    pub id: String,
    /// Authored scene name.
    pub name: String,
    /// Entity rows in source order.
    pub entities: Vec<EditorEntity>,
}

#[derive(Clone, Debug)]
struct PendingEdit {
    operation: SceneEditOperation,
    original_sha256: String,
    candidate_sha256: String,
}

/// File-backed editor session; project validation and writes share the CLI's public services.
pub struct EditorSession {
    project_root: PathBuf,
    project_name: String,
    scenes: Vec<EditorScene>,
    scene_index: usize,
    selected_entity: usize,
    resources: Vec<EditorResource>,
    pending: Option<PendingEdit>,
    pending_scene_file_override: Option<String>,
}

impl EditorSession {
    /// Opens a project after the same strict validation performed by `hycel check`.
    ///
    /// # Errors
    ///
    /// Returns an error when the project path or authored files are invalid or exceed editor bounds.
    pub fn open(project_path: &Path) -> Result<Self, String> {
        let project_root = project_path
            .canonicalize()
            .map_err(|error| format!("cannot resolve project root: {error}"))?;
        if !project_root.is_dir() {
            return Err("project root must be a directory".to_owned());
        }
        let project_name = Self::validate_and_name(&project_root)?;
        let scenes = load_scenes(&project_root)?;
        let resources = inspect_resources(&project_root)?;
        if scenes.is_empty() {
            return Err("project contains no scenes to edit".to_owned());
        }
        Ok(Self {
            project_root,
            project_name,
            scenes,
            scene_index: 0,
            selected_entity: 0,
            resources,
            pending: None,
            pending_scene_file_override: None,
        })
    }

    /// Project title used in the editor header.
    #[must_use]
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Canonical project root, used only to start the compiled-in game runtime.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Validated scene index.
    #[must_use]
    pub fn scenes(&self) -> &[EditorScene] {
        &self.scenes
    }

    /// Current scene, if it has been loaded.
    #[must_use]
    pub fn current_scene(&self) -> &EditorScene {
        &self.scenes[self.scene_index]
    }

    /// Current scene index.
    #[must_use]
    pub const fn scene_index(&self) -> usize {
        self.scene_index
    }

    /// Selected entity index.
    #[must_use]
    pub const fn selected_entity_index(&self) -> usize {
        self.selected_entity
    }

    /// Selected entity, or none for an empty scene.
    #[must_use]
    pub fn selected_entity(&self) -> Option<&EditorEntity> {
        self.current_scene().entities.get(self.selected_entity)
    }

    /// Resources and direct dependencies from the shared inspection service.
    #[must_use]
    pub fn resources(&self) -> &[EditorResource] {
        &self.resources
    }

    /// Number of project resources from the shared inspection service.
    #[must_use]
    pub fn resource_count(&self) -> usize {
        self.resources.len()
    }

    /// Animation resources in deterministic project-inspection order.
    pub fn animation_resources(&self) -> impl Iterator<Item = &EditorResource> + '_ {
        self.resources
            .iter()
            .filter(|resource| resource.kind == "animation")
    }

    /// Inspect one resource's bounded source hash and any validated cached import records.
    ///
    /// # Errors
    ///
    /// Returns an error for missing/invalid source data, unsafe cache paths, malformed import
    /// records, or directories that exceed the editor's inspection bounds.
    pub fn inspect_resource_import(
        &self,
        resource_index: usize,
    ) -> Result<EditorResourceImportStatus, String> {
        let resource = self
            .resources
            .get(resource_index)
            .ok_or_else(|| "resource selection is outside the project resource list".to_owned())?;
        let descriptor_path =
            resolve_existing_project_path(&self.project_root, &resource.descriptor_file)
                .map_err(|diagnostic| diagnostic.to_string())?;
        let descriptor_bytes = read_bounded_file(&descriptor_path, MAX_DOCUMENT_BYTES)?;
        let mut registry = ResourceRegistry::default();
        registry
            .register("texture", std::iter::empty::<&str>())
            .and_then(|()| registry.register("animation", std::iter::empty::<&str>()))
            .map_err(|error| error.to_string())?;
        let descriptor = ResourceDescriptor::parse_json_with_registry(
            &descriptor_bytes,
            &resource.descriptor_file,
            &registry,
        )
        .map_err(|diagnostics| {
            diagnostics
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        if descriptor.id() != resource.id
            || descriptor.kind() != resource.kind
            || descriptor.source() != resource.source
        {
            return Err("resource descriptor changed since the editor opened".to_owned());
        }
        let source_hash = hash_source_file(
            &self.project_root,
            descriptor.source(),
            MAX_RESOURCE_SOURCE_BYTES,
        )
        .map_err(|error| error.to_string())?;
        let records = read_cached_import_records(&self.project_root, &descriptor, source_hash)?;
        Ok(EditorResourceImportStatus {
            source_sha256: source_hash.to_hex(),
            records,
        })
    }

    /// Whether a staged, unapplied change exists.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.pending.is_some()
    }

    /// Advance the selection with wraparound; returns false for an empty scene.
    pub fn select_next_entity(&mut self, backwards: bool) -> bool {
        if self.is_dirty() {
            return false;
        }
        let count = self.current_scene().entities.len();
        if count == 0 {
            self.selected_entity = 0;
            return false;
        }
        if backwards {
            self.selected_entity = if self.selected_entity == 0 {
                count - 1
            } else {
                self.selected_entity - 1
            };
        } else {
            self.selected_entity = (self.selected_entity + 1) % count;
        }
        true
    }

    /// Selects an entity by stable UUID.
    pub fn select_entity(&mut self, entity_id: &str) -> bool {
        if self.is_dirty() {
            return false;
        }
        let Some(index) = self
            .current_scene()
            .entities
            .iter()
            .position(|entity| entity.id == entity_id)
        else {
            return false;
        };
        self.selected_entity = index;
        true
    }

    /// Change scenes while preserving pending edits only by refusing the transition.
    ///
    /// # Errors
    ///
    /// Returns an error if a staged edit would be abandoned.
    pub fn select_next_scene(&mut self, backwards: bool) -> Result<(), String> {
        if self.is_dirty() {
            return Err("save or discard the staged edit before changing scenes".to_owned());
        }
        let count = self.scenes.len();
        if backwards {
            self.scene_index = if self.scene_index == 0 {
                count - 1
            } else {
                self.scene_index - 1
            };
        } else {
            self.scene_index = (self.scene_index + 1) % count;
        }
        self.selected_entity = 0;
        Ok(())
    }

    /// Stage a bounded fixed-size movement operation for the selected entity.
    ///
    /// # Errors
    ///
    /// Returns an error if no entity is selected, the staged operation conflicts, or arithmetic overflows.
    pub fn stage_move(&mut self, delta_milli: [i64; 2]) -> Result<(), String> {
        let entity = self
            .selected_entity()
            .ok_or_else(|| "select an entity before moving it".to_owned())?;
        let translation = match &self.pending {
            Some(PendingEdit {
                operation:
                    SceneEditOperation::SetEntityTransform {
                        entity_id,
                        translation_milli: Some(translation),
                        ..
                    },
                ..
            }) if entity_id == &entity.id => *translation,
            Some(_) => return Err("save or discard the current staged operation first".to_owned()),
            None => entity.translation_milli,
        };
        let translation = [
            translation[0]
                .checked_add(delta_milli[0])
                .ok_or_else(|| "horizontal translation overflow".to_owned())?,
            translation[1]
                .checked_add(delta_milli[1])
                .ok_or_else(|| "vertical translation overflow".to_owned())?,
        ];
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::SetEntityTransform {
                entity_id: entity.id.clone(),
                translation_milli: Some(translation),
                rotation_units: None,
                scale_milli: None,
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Stage a typed entity rename (maximum 128 Unicode characters).
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty/too long, no entity is selected, or another operation is staged.
    pub fn stage_rename(&mut self, name: String) -> Result<(), String> {
        if name.is_empty() || name.chars().count() > 128 {
            return Err("entity name must contain 1..=128 characters".to_owned());
        }
        let entity = self
            .selected_entity()
            .ok_or_else(|| "select an entity before renaming it".to_owned())?;
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::RenameEntity {
                entity_id: entity.id.clone(),
                name,
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Stage a toggle for one lowercase ASCII entity tag.
    ///
    /// # Errors
    ///
    /// Returns an error when the tag is invalid, no entity is selected, or another operation is staged.
    pub fn stage_toggle_entity_tag(&mut self, tag: String) -> Result<(), String> {
        if tag.is_empty()
            || tag.len() > 64
            || !tag
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err("tag must contain 1..=64 lowercase ASCII letters or digits".to_owned());
        }
        let entity = self
            .selected_entity()
            .ok_or_else(|| "select an entity before changing its tags".to_owned())?;
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        let mut tags = entity.tags.clone();
        if let Some(index) = tags.iter().position(|current| current == &tag) {
            tags.remove(index);
        } else {
            if tags.len() >= 64 {
                return Err("selected entity already has the maximum of 64 tags".to_owned());
            }
            tags.push(tag);
        }
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::SetEntityTags {
                entity_id: entity.id.clone(),
                tags,
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Stage a typed current-scene rename (maximum 128 Unicode characters).
    ///
    /// # Errors
    ///
    /// Returns an error if the name is empty/too long or another operation is staged.
    pub fn stage_rename_scene(&mut self, name: String) -> Result<(), String> {
        if name.is_empty() || name.chars().count() > 128 {
            return Err("scene name must contain 1..=128 characters".to_owned());
        }
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::RenameScene { name },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Stage a new empty entity with a random RFC 4122 version-4 UUID.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation is already staged or the OS random source fails.
    pub fn stage_create_entity(&mut self, name: String) -> Result<String, String> {
        if name.is_empty() || name.chars().count() > 128 {
            return Err("entity name must contain 1..=128 characters".to_owned());
        }
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        let entity_id = random_uuid()?;
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::CreateEntity {
                entity_id: entity_id.clone(),
                name,
                translation_milli: [0, 0],
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(entity_id)
    }

    /// Stage deletion of the selected entity; project validation protects references.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation is staged or the scene has no selected entity.
    pub fn stage_delete_entity(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        let entity_id = self
            .selected_entity()
            .map(|entity| entity.id.clone())
            .ok_or_else(|| "select an entity before deleting it".to_owned())?;
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::DeleteEntity { entity_id },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Stage removal of one present registered component from the selected entity.
    ///
    /// # Errors
    ///
    /// Returns an error if an operation is staged or the selected entity lacks the component.
    pub fn stage_remove_component(&mut self, component_type: &str) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        let entity_id = self
            .selected_entity()
            .filter(|entity| {
                entity
                    .component_types
                    .iter()
                    .any(|kind| kind == component_type)
            })
            .map(|entity| entity.id.clone())
            .ok_or_else(|| "selected entity does not have that component".to_owned())?;
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::RemoveEntityComponent {
                entity_id,
                component_type: component_type.to_owned(),
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Stage an animation component using the first authored animation resource.
    ///
    /// # Errors
    ///
    /// Returns an error if the selection is empty, an operation is staged, or there is no animation resource.
    pub fn stage_animation_component(&mut self) -> Result<(), String> {
        self.stage_animation_component_at(0)
    }

    /// Stage an animation component using the indexed animation resource.
    ///
    /// # Errors
    ///
    /// Returns an error if the selection is empty, an operation is staged, or the resource index is invalid.
    pub fn stage_animation_component_at(&mut self, resource_index: usize) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        let entity_id = self
            .selected_entity()
            .map(|entity| entity.id.clone())
            .ok_or_else(|| "select an entity before adding a component".to_owned())?;
        let clip_id = self
            .animation_resources()
            .nth(resource_index)
            .map(|resource| resource.id.clone())
            .ok_or_else(|| {
                if self.animation_resources().next().is_some() {
                    "animation resource selection is outside the project list".to_owned()
                } else {
                    "project has no animation resource to attach".to_owned()
                }
            })?;
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::SetEntityComponent {
                entity_id,
                component_type: "hycel.animation".to_owned(),
                schema_version: 1,
                data: BTreeMap::from([("clip_id".to_owned(), Value::String(clip_id))]),
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        Ok(())
    }

    /// Current staged operation for rendering and status feedback.
    #[must_use]
    pub fn pending_operation(&self) -> Option<&SceneEditOperation> {
        self.pending.as_ref().map(|pending| &pending.operation)
    }

    /// Stage an empty scene at a fresh project-relative path.
    ///
    /// # Errors
    ///
    /// Returns an error when an operation is already staged or OS entropy is unavailable.
    pub fn stage_create_scene(&mut self, name: String) -> Result<String, String> {
        if name.is_empty() || name.chars().count() > 128 {
            return Err("scene name must contain 1..=128 characters".to_owned());
        }
        if self.pending.is_some() {
            return Err("save or discard the current staged operation first".to_owned());
        }
        if self.scenes.len() >= MAX_SCENES {
            return Err(format!(
                "editor scene list is already at its {MAX_SCENES}-scene limit"
            ));
        }
        let scene_id = random_uuid()?;
        let relative_path = format!("scenes/room-{scene_id}.json");
        self.pending = Some(PendingEdit {
            operation: SceneEditOperation::CreateScene {
                scene_id: scene_id.clone(),
                name,
            },
            original_sha256: String::new(),
            candidate_sha256: String::new(),
        });
        self.pending_scene_file_override = Some(relative_path);
        Ok(scene_id)
    }

    /// Preview the staged typed operation and retain its source/candidate identity.
    ///
    /// # Errors
    ///
    /// Returns an error if no operation is staged or the shared project service rejects the edit.
    pub fn preview(&mut self) -> Result<SceneEditPreview, String> {
        let scene_file = self
            .pending_scene_file_override
            .clone()
            .unwrap_or_else(|| self.current_scene().relative_path.clone());
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| "there is no staged change to preview".to_owned())?;
        let preview = preview_scene_edit(&self.project_root, &scene_file, &pending.operation)?;
        pending.original_sha256.clone_from(&preview.original_sha256);
        pending
            .candidate_sha256
            .clone_from(&preview.candidate_sha256);
        Ok(preview)
    }

    /// Apply a second-preview-verified change after explicit keyboard confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error if the preview is absent/stale, the shared service rejects the write, or reloading fails.
    pub fn apply_preview(&mut self) -> Result<SceneEditReceipt, String> {
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| "there is no staged change to apply".to_owned())?;
        if pending.original_sha256.is_empty() || pending.candidate_sha256.is_empty() {
            return Err("preview the staged change before saving it".to_owned());
        }
        let scene_file = self
            .pending_scene_file_override
            .clone()
            .unwrap_or_else(|| self.current_scene().relative_path.clone());
        let operation = pending.operation.clone();
        let expected_original = pending.original_sha256.clone();
        let expected_candidate = pending.candidate_sha256.clone();
        let selected_after_save = match &operation {
            SceneEditOperation::CreateEntity { entity_id, .. } => Some(entity_id.clone()),
            _ => None,
        };
        let scene_to_select_after_save =
            matches!(&operation, SceneEditOperation::CreateScene { .. })
                .then(|| scene_file.clone());
        let current = preview_scene_edit(&self.project_root, &scene_file, &operation)?;
        if current.original_sha256 != expected_original
            || current.candidate_sha256 != expected_candidate
        {
            return Err("scene changed after preview; preview again before saving".to_owned());
        }
        let receipt = apply_scene_edit(
            &self.project_root,
            &scene_file,
            &operation,
            &expected_original,
        )?;
        self.reload()?;
        if let Some(relative_path) = scene_to_select_after_save {
            if let Some(index) = self
                .scenes
                .iter()
                .position(|scene| scene.relative_path == relative_path)
            {
                self.scene_index = index;
                self.selected_entity = 0;
            }
        }
        if let Some(entity_id) = selected_after_save {
            let _ = self.select_entity(&entity_id);
        }
        Ok(receipt)
    }

    /// Discard unsaved staging and reload all scene data from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if project validation or reloading fails.
    pub fn discard(&mut self) -> Result<(), String> {
        self.reload()
    }

    /// Run all bounded CLI reference-game scenarios and retain each outcome for display.
    ///
    /// # Errors
    ///
    /// Returns an error when the CLI cannot produce a bounded, internally consistent report.
    pub fn run_tests_detailed(&self) -> Result<EditorTestReport, String> {
        let root = self
            .project_root
            .to_str()
            .ok_or("project root must be UTF-8")?;
        let result = execute(["test", root, "--json"].map(Into::into));
        parse_test_report(&result.stdout, result.exit_code)
    }

    /// Run all bounded CLI reference-game scenarios and return their counts.
    ///
    /// # Errors
    ///
    /// Returns an error with scenario details when any test fails or the report is malformed.
    pub fn run_tests(&self) -> Result<(usize, usize), String> {
        let report = self.run_tests_detailed()?;
        if report.failed_count == 0 {
            Ok((report.passed_count, report.failed_count))
        } else {
            let failures = report
                .tests
                .iter()
                .filter(|test| !test.passed)
                .map(|test| format!("{}: {}", test.name, test.message))
                .collect::<Vec<_>>()
                .join("; ");
            Err(format!(
                "{} scenario(s) failed: {failures}",
                report.failed_count
            ))
        }
    }

    /// Write a unique bounded JSON test report under `.hycel/editor-previews/`.
    ///
    /// # Errors
    ///
    /// Returns an error if the report is invalid/oversized or generated output cannot be safely published.
    pub fn export_test_report(&self, report: &EditorTestReport) -> Result<PathBuf, String> {
        validate_test_report(report)?;
        let artifact = serde_json::json!({
            "schema_version": EDITOR_TEST_REPORT_SCHEMA_VERSION,
            "report": report,
        });
        let bytes = serde_json::to_vec(&artifact)
            .map_err(|error| format!("cannot serialize test report: {error}"))?;
        if bytes.len() > MAX_TEST_REPORT_BYTES {
            return Err(format!(
                "test report exceeds the {MAX_TEST_REPORT_BYTES}-byte artifact limit"
            ));
        }
        let _export_guard = TEST_REPORT_EXPORT_LOCK
            .lock()
            .map_err(|_| "test-report export lock is poisoned".to_owned())?;
        let cache = self.project_root.join(".hycel");
        ensure_generated_directory(&self.project_root, &cache)?;
        let previews = cache.join("editor-previews");
        ensure_generated_directory(&self.project_root, &previews)?;
        ensure_test_report_capacity(&previews)?;
        let id = random_uuid()?;
        let output = previews.join(format!("test-report-{id}.json"));
        let staging = previews.join(format!(".test-report-{id}.tmp"));
        publish_new_artifact(&staging, &output, &bytes)?;
        Ok(output)
    }

    /// Export a deterministic authored-scene SVG into `.hycel/editor-previews/`.
    ///
    /// # Errors
    ///
    /// Returns an error when the generated directory is unsafe or the CLI cannot write the preview.
    pub fn export_screenshot(&self) -> Result<PathBuf, String> {
        let cache = self.project_root.join(".hycel");
        ensure_generated_directory(&self.project_root, &cache)?;
        let previews = cache.join("editor-previews");
        ensure_generated_directory(&self.project_root, &previews)?;
        let unique = random_uuid()?;
        let output = previews.join(format!("{}-{unique}.svg", self.current_scene().id));
        let root = self
            .project_root
            .to_str()
            .ok_or("project root must be UTF-8")?;
        let scene_id = &self.current_scene().id;
        let output_text = output.to_str().ok_or("preview output path must be UTF-8")?;
        let result = execute(
            [
                "screenshot",
                root,
                "--scene",
                scene_id,
                "--output",
                output_text,
                "--json",
            ]
            .map(Into::into),
        );
        if result.exit_code == 0 {
            Ok(output)
        } else {
            Err(result.stdout)
        }
    }

    /// Run the CLI's project validation and update the diagnostic status.
    ///
    /// # Errors
    ///
    /// Returns the shared CLI error envelope when project validation fails.
    pub fn check(&self) -> Result<(), String> {
        let root = self
            .project_root
            .to_str()
            .ok_or("project root must be UTF-8")?;
        let result = execute(["check", root, "--json"].map(Into::into));
        if result.exit_code == 0 {
            Ok(())
        } else {
            Err(result.stdout)
        }
    }

    fn validate_and_name(root: &Path) -> Result<String, String> {
        let root_text = root.to_str().ok_or("project root must be UTF-8")?;
        let result = execute(["check", root_text, "--json"].map(Into::into));
        if result.exit_code != 0 {
            return Err(format!("project validation failed: {}", result.stdout));
        }
        let inspected = execute(["inspect", root_text, "--json"].map(Into::into));
        if inspected.exit_code != 0 {
            return Err(inspected.stdout);
        }
        serde_json::from_str::<Value>(&inspected.stdout)
            .ok()
            .and_then(|value| {
                value
                    .get("result")?
                    .get("project_name")?
                    .as_str()
                    .map(str::to_owned)
            })
            .ok_or_else(|| "project inspection omitted its name".to_owned())
    }

    fn reload(&mut self) -> Result<(), String> {
        let project_name = Self::validate_and_name(&self.project_root)?;
        let scenes = load_scenes(&self.project_root)?;
        let resources = inspect_resources(&self.project_root)?;
        self.scenes = scenes;
        self.project_name = project_name;
        self.resources = resources;
        self.scene_index = self.scene_index.min(self.scenes.len().saturating_sub(1));
        self.selected_entity = self
            .selected_entity
            .min(self.current_scene().entities.len().saturating_sub(1));
        self.pending = None;
        self.pending_scene_file_override = None;
        Ok(())
    }
}

fn random_uuid() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| format!("OS random source failed: {error}"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

fn ensure_generated_directory(root: &Path, path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!(
                "generated directory {} must be a real directory",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                format!(
                    "cannot create generated directory {}: {error}",
                    path.display()
                )
            })?;
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect generated directory {}: {error}",
                path.display()
            ));
        }
    }
    let canonical = path.canonicalize().map_err(|error| {
        format!(
            "cannot resolve generated directory {}: {error}",
            path.display()
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "generated directory {} escaped the project root",
            path.display()
        ));
    }
    Ok(())
}

fn validate_test_report(report: &EditorTestReport) -> Result<(), String> {
    if report.tests.is_empty() || report.tests.len() > MAX_TEST_CASES {
        return Err(format!(
            "test report must contain 1..={MAX_TEST_CASES} cases"
        ));
    }
    for test in &report.tests {
        if test.name.is_empty()
            || test.name.chars().count() > MAX_TEST_NAME_CHARS
            || test.message.chars().count() > MAX_TEST_MESSAGE_CHARS
        {
            return Err("test report contains an empty or oversized field".to_owned());
        }
    }
    let passed_count = report.tests.iter().filter(|test| test.passed).count();
    let failed_count = report.tests.len() - passed_count;
    if report.passed_count != passed_count || report.failed_count != failed_count {
        return Err("test report counts do not match its cases".to_owned());
    }
    Ok(())
}

fn ensure_test_report_capacity(previews: &Path) -> Result<(), String> {
    let entries = fs::read_dir(previews)
        .map_err(|error| format!("cannot inspect test-report directory: {error}"))?;
    let mut entry_count = 0;
    let mut report_count = 0;
    for entry in entries {
        entry_count += 1;
        if entry_count > MAX_PREVIEW_DIRECTORY_ENTRIES {
            return Err(format!(
                "editor preview directory exceeds its {MAX_PREVIEW_DIRECTORY_ENTRIES}-entry scan limit"
            ));
        }
        let entry = entry.map_err(|error| format!("cannot inspect preview entry: {error}"))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("test-report-")
            && name.to_string_lossy().ends_with(".json")
        {
            report_count += 1;
            if report_count >= MAX_TEST_REPORT_ARTIFACTS {
                return Err(format!(
                    "editor already has {MAX_TEST_REPORT_ARTIFACTS} test-report artifacts; remove old reports before exporting another"
                ));
            }
        }
    }
    Ok(())
}

fn publish_new_artifact(staging: &Path, output: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging)
        .map_err(|error| format!("cannot create test-report staging file: {error}"))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let cleanup = fs::remove_file(staging);
        return Err(match cleanup {
            Ok(()) => format!("cannot write test-report staging file: {error}"),
            Err(cleanup_error) => format!(
                "cannot write test-report staging file: {error}; cleanup also failed: {cleanup_error}"
            ),
        });
    }
    drop(file);
    if let Err(error) = fs::hard_link(staging, output) {
        let cleanup = fs::remove_file(staging);
        return Err(match cleanup {
            Ok(()) => format!("cannot publish test report without overwriting: {error}"),
            Err(cleanup_error) => format!(
                "cannot publish test report without overwriting: {error}; cleanup also failed: {cleanup_error}"
            ),
        });
    }
    fs::remove_file(staging).map_err(|error| {
        format!(
            "test report was published at {} but staging cleanup failed: {error}",
            output.display()
        )
    })
}

fn parse_test_report(stdout: &str, exit_code: i32) -> Result<EditorTestReport, String> {
    let response: Value = serde_json::from_str(stdout)
        .map_err(|error| format!("cannot parse test results: {error}"))?;
    if response["schema_version"].as_u64() != Some(1)
        || response["command"].as_str() != Some("test")
    {
        return Err("test response has an unsupported CLI envelope".to_owned());
    }
    let Some(result) = response.get("result").filter(|value| !value.is_null()) else {
        if response["ok"].as_bool() == Some(false) && exit_code != 0 {
            return Err(cli_test_diagnostic_summary(&response["diagnostics"]));
        }
        return Err("test response omitted its cases".to_owned());
    };
    let rows = result["tests"]
        .as_array()
        .ok_or_else(|| "test response omitted its cases".to_owned())?;
    if rows.is_empty() || rows.len() > MAX_TEST_CASES {
        return Err(format!(
            "test response case count must be 1..={MAX_TEST_CASES}"
        ));
    }
    let tests = rows
        .iter()
        .map(|row| {
            let name = row["name"]
                .as_str()
                .ok_or_else(|| "test case omitted its name".to_owned())?;
            let message = row["message"]
                .as_str()
                .ok_or_else(|| "test case omitted its outcome message".to_owned())?;
            if name.is_empty()
                || name.chars().count() > MAX_TEST_NAME_CHARS
                || message.chars().count() > MAX_TEST_MESSAGE_CHARS
            {
                return Err("test case name or message exceeds its display limit".to_owned());
            }
            Ok(EditorTestCase {
                name: name.to_owned(),
                passed: row["passed"]
                    .as_bool()
                    .ok_or_else(|| "test case omitted its passed flag".to_owned())?,
                message: message.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let passed_count = tests.iter().filter(|test| test.passed).count();
    let failed_count = tests.len() - passed_count;
    if result["passed_count"].as_u64() != Some(passed_count as u64)
        || result["failed_count"].as_u64() != Some(failed_count as u64)
        || response["ok"].as_bool() != Some(failed_count == 0)
        || (exit_code == 0) != (failed_count == 0)
    {
        return Err("test response counts or exit status do not match its cases".to_owned());
    }
    Ok(EditorTestReport {
        tests,
        passed_count,
        failed_count,
    })
}

fn cli_test_diagnostic_summary(diagnostics: &Value) -> String {
    let details = diagnostics
        .as_array()
        .into_iter()
        .flatten()
        .take(3)
        .filter_map(|diagnostic| {
            let code = diagnostic["code"].as_str()?;
            let message = diagnostic["message"].as_str()?;
            let path = diagnostic["path"].as_str().unwrap_or("test");
            Some(format!(
                "{} {}: {}",
                short_text(code, 32),
                short_text(path, 96),
                short_text(message, 256)
            ))
        })
        .collect::<Vec<_>>();
    if details.is_empty() {
        "reference tests could not run; CLI returned no diagnostic details".to_owned()
    } else {
        short_text(&details.join("; "), 1_024)
    }
}

fn short_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn inspect_resources(root: &Path) -> Result<Vec<EditorResource>, String> {
    let root_text = root.to_str().ok_or("project root must be UTF-8")?;
    let mut resources = Vec::new();
    let mut offset = 0_usize;
    let mut total = None;
    loop {
        let output = execute(
            [
                "inspect".to_owned(),
                root_text.to_owned(),
                "--offset".to_owned(),
                offset.to_string(),
                "--limit".to_owned(),
                "1000".to_owned(),
                "--json".to_owned(),
            ]
            .into_iter()
            .map(Into::into),
        );
        if output.exit_code != 0 {
            return Err(output.stdout);
        }
        let response: Value =
            serde_json::from_str(&output.stdout).map_err(|error| error.to_string())?;
        let result = &response["result"];
        let page_total = result["total_resource_count"]
            .as_u64()
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| "project inspection omitted resource count".to_owned())?;
        if page_total > MAX_RESOURCES || total.is_some_and(|known| known != page_total) {
            return Err(format!(
                "editor supports a stable resource index up to {MAX_RESOURCES} entries"
            ));
        }
        total = Some(page_total);
        let page = result["resources"]
            .as_array()
            .ok_or_else(|| "project inspection omitted resource page".to_owned())?;
        let dependencies = &result["dependencies"]["resources"];
        for resource in page {
            let text = |field: &str| {
                resource[field]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("project inspection resource omitted {field}"))
            };
            let id = text("id")?;
            let dependency_count = dependencies[&id]["resource_ids"]
                .as_array()
                .map_or(0, Vec::len);
            resources.push(EditorResource {
                id,
                kind: text("kind")?,
                descriptor_file: text("descriptor_file")?,
                source: text("source")?,
                dependency_count,
            });
        }
        if resources.len() == page_total {
            return Ok(resources);
        }
        if page.is_empty() {
            return Err(
                "project inspection returned an empty resource page before the end".to_owned(),
            );
        }
        offset = resources.len();
    }
}

fn read_bounded_file(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "{} must be a regular non-symlink file",
            path.display()
        ));
    }
    if metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(format!(
            "{} exceeds the {maximum}-byte limit",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(maximum.min(64 * 1024));
    fs::File::open(path)
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

fn cache_directory_exists(root: &Path, relative: &Path) -> Result<bool, String> {
    let mut current = root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err("generated import cache path contains a non-normal component".to_owned());
        };
        current.push(part);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!("cannot inspect {}: {error}", current.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "generated cache path {} must contain only real directories",
                current.display()
            ));
        }
    }
    Ok(true)
}

fn read_cached_import_records(
    root: &Path,
    descriptor: &ResourceDescriptor,
    source_hash: hycel_assets::ContentHash,
) -> Result<Vec<EditorImportRecordStatus>, String> {
    let cache_root = Path::new(".hycel").join("imports").join(descriptor.id());
    if !cache_directory_exists(root, &cache_root)? {
        return Ok(Vec::new());
    }
    let directory = root.join(&cache_root);
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        .take(MAX_IMPORT_RECORDS_PER_RESOURCE + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate {}: {error}", directory.display()))?;
    if entries.len() > MAX_IMPORT_RECORDS_PER_RESOURCE {
        return Err(format!(
            "resource import cache exceeds {MAX_IMPORT_RECORDS_PER_RESOURCE} fingerprints"
        ));
    }
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "import cache entry {} must be a real fingerprint directory",
                path.display()
            ));
        }
        let fingerprint = entry
            .file_name()
            .into_string()
            .map_err(|_| "import cache contains a non-UTF-8 fingerprint name".to_owned())?;
        if fingerprint.len() != 64
            || !fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "invalid import fingerprint directory {fingerprint:?}"
            ));
        }
        let record_path = path.join("record.json");
        let record_bytes = read_bounded_file(&record_path, hycel_assets::MAX_IMPORT_RECORD_BYTES)?;
        let record = ImportRecord::parse_json(&record_bytes).map_err(|error| error.to_string())?;
        if record.resource_id() != descriptor.id()
            || record.fingerprint_sha256() != fingerprint
            || root.join(record.cache_relative_path()) != path
        {
            return Err(format!(
                "import record {} does not match its resource/fingerprint cache path",
                record_path.display()
            ));
        }
        let (_, importer_version) = record.importer();
        let desired = ImportRecord::new(descriptor, source_hash, importer_version)
            .map_err(|error| error.to_string())?;
        let planned_decision = plan_reimport(Some(&record), &desired);
        let output_entries = fs::read_dir(&path)
            .map_err(|error| format!("cannot enumerate {}: {error}", path.display()))?
            .take(257)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("cannot enumerate {}: {error}", path.display()))?;
        if output_entries.len() > 256 {
            return Err(format!(
                "import cache fingerprint {fingerprint} exceeds the 256-entry inspection limit"
            ));
        }
        let mut has_derived_output = false;
        for output in output_entries {
            if output.file_name() == "record.json" {
                continue;
            }
            let metadata = fs::symlink_metadata(output.path())
                .map_err(|error| format!("cannot inspect {}: {error}", output.path().display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "cached output {} must be a regular non-symlink file",
                    output.path().display()
                ));
            }
            has_derived_output = true;
        }
        let decision =
            if !has_derived_output && planned_decision == ReimportDecision::ReuseCachedOutput {
                ReimportDecision::Import(hycel_assets::ReimportReason::CachedOutputMissing)
            } else {
                planned_decision
            };
        records.push(EditorImportRecordStatus {
            fingerprint_sha256: fingerprint,
            importer_version: importer_version.to_owned(),
            decision_using_recorded_version: decision,
            has_derived_output,
        });
    }
    records.sort_by(|left, right| left.fingerprint_sha256.cmp(&right.fingerprint_sha256));
    Ok(records)
}

fn load_scenes(root: &Path) -> Result<Vec<EditorScene>, String> {
    let directory = root.join("scenes");
    let mut files = fs::read_dir(&directory)
        .map_err(|error| format!("cannot read scenes directory: {error}"))?
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by_key(fs::DirEntry::file_name);
    let mut scenes = Vec::new();
    for entry in files {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        if scenes.len() >= MAX_SCENES {
            return Err(format!("editor scene list exceeds {MAX_SCENES} files"));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "scene {} must be a regular non-symlink file",
                path.display()
            ));
        }
        if metadata.len() > MAX_DOCUMENT_BYTES as u64 {
            return Err(format!(
                "scene {} exceeds the project document limit",
                path.display()
            ));
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
        if !canonical.starts_with(root) {
            return Err(format!("scene {} escaped the project root", path.display()));
        }
        let bytes = fs::read(&canonical)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let document: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid validated scene {}: {error}", path.display()))?;
        scenes.push(scene_from_json(&path, root, &document)?);
    }
    if scenes.is_empty() {
        return Err("project has no .json scene files".to_owned());
    }
    Ok(scenes)
}

fn scene_from_json(path: &Path, root: &Path, document: &Value) -> Result<EditorScene, String> {
    let string = |value: &Value, field: &str| {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("validated scene is missing string {field}"))
    };
    let entities_value = document["entities"]
        .as_array()
        .ok_or_else(|| "validated scene is missing its entity list".to_owned())?;
    if entities_value.len() > MAX_ENTITIES_PER_SCENE {
        return Err(format!(
            "editor viewport supports at most {MAX_ENTITIES_PER_SCENE} entities per scene"
        ));
    }
    let mut entities = Vec::with_capacity(entities_value.len());
    for value in entities_value {
        let translation = pair_i64(&value["transform"]["translation_milli"], [0, 0])?;
        let scale = pair_i64(&value["transform"]["scale_milli"], [1_000, 1_000])?;
        let tags = value["tags"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|tag| string(tag, "entity.tag"))
            .collect::<Result<Vec<_>, _>>()?;
        let component_types = value["components"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|component| string(&component["type"], "component.type"))
            .collect::<Result<Vec<_>, _>>()?;
        entities.push(EditorEntity {
            id: string(&value["id"], "entity.id")?,
            name: string(&value["name"], "entity.name")?,
            translation_milli: translation,
            scale_milli: scale,
            tags,
            component_types,
        });
    }
    Ok(EditorScene {
        relative_path: path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/"),
        id: string(&document["id"], "scene.id")?,
        name: string(&document["name"], "scene.name")?,
        entities,
    })
}

fn pair_i64(value: &Value, default: [i64; 2]) -> Result<[i64; 2], String> {
    let Some(array) = value.as_array() else {
        return Ok(default);
    };
    if array.len() != 2 {
        return Err("validated transform pair must have exactly two values".to_owned());
    }
    Ok([
        array[0]
            .as_i64()
            .ok_or_else(|| "transform values must be signed integers".to_owned())?,
        array[1]
            .as_i64()
            .ok_or_else(|| "transform values must be signed integers".to_owned())?,
    ])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        EditorSession, EditorTestCase, EditorTestReport, MAX_RESOURCE_SOURCE_BYTES,
        MAX_TEST_REPORT_ARTIFACTS, parse_test_report, publish_new_artifact,
    };
    use hycel_cli::execute;
    use serde_json::{Value, json};

    #[test]
    fn detailed_test_report_preserves_bounded_failure_artifacts() {
        let response = json!({
            "schema_version": 1,
            "command": "test",
            "ok": false,
            "result": {
                "tests": [{
                    "name": "echo-flight",
                    "passed": false,
                    "message": "the authored echo did not open the gate"
                }],
                "passed_count": 0,
                "failed_count": 1
            }
        });
        let report = parse_test_report(&response.to_string(), 1).unwrap();
        assert_eq!(report.passed_count, 0);
        assert_eq!(report.failed_count, 1);
        assert_eq!(report.tests[0].name, "echo-flight");
        assert_eq!(
            report.tests[0].message,
            "the authored echo did not open the gate"
        );
        assert!(parse_test_report(&response.to_string(), 0).is_err());
        let mut wrong_command = response.clone();
        wrong_command["command"] = Value::String("inspect".to_owned());
        assert!(parse_test_report(&wrong_command.to_string(), 1).is_err());
        let mut wrong_version = response.clone();
        wrong_version["schema_version"] = json!(2);
        assert!(parse_test_report(&wrong_version.to_string(), 1).is_err());
        let mut mismatched_ok = response.clone();
        mismatched_ok["ok"] = json!(true);
        assert!(parse_test_report(&mismatched_ok.to_string(), 1).is_err());

        let cli_error = json!({
            "schema_version": 1,
            "command": "test",
            "ok": false,
            "result": null,
            "diagnostics": [{
                "code": "HYCEL-CLI-121",
                "path": "tests.content",
                "message": format!("reference game content is missing {}", "x".repeat(2_048))
            }]
        });
        let error = parse_test_report(&cli_error.to_string(), 1).unwrap_err();
        let message_prefix = "reference game content is missing ";
        let bounded_message = format!(
            "{message_prefix}{}",
            "x".repeat(256 - message_prefix.chars().count())
        );
        assert_eq!(
            error,
            format!("HYCEL-CLI-121 tests.content: {bounded_message}")
        );
        assert!(error.chars().count() <= 400);
    }

    #[test]
    fn artifact_publication_collision_preserves_destination_and_cleans_staging() {
        let directory = temporary_directory();
        let staging = directory.join("report.tmp");
        let output = directory.join("report.json");
        let original = b"existing report";
        fs::write(&output, original).unwrap();

        let error = publish_new_artifact(&staging, &output, b"new report").unwrap_err();
        assert!(error.contains("cannot publish test report without overwriting"));
        assert_eq!(fs::read(&output).unwrap(), original);
        assert!(!staging.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn test_report_export_refuses_to_exceed_retention_cap() {
        let project = temporary_project();
        let editor = EditorSession::open(&project).unwrap();
        let previews = editor.project_root().join(".hycel/editor-previews");
        fs::create_dir_all(&previews).unwrap();
        for index in 0..MAX_TEST_REPORT_ARTIFACTS {
            fs::write(
                previews.join(format!("test-report-{index}.json")),
                b"prior report",
            )
            .unwrap();
        }
        let report = EditorTestReport {
            tests: vec![EditorTestCase {
                name: "scenario".to_owned(),
                passed: true,
                message: "passed".to_owned(),
            }],
            passed_count: 1,
            failed_count: 0,
        };
        let error = editor.export_test_report(&report).unwrap_err();
        assert!(error.contains("remove old reports"));
        assert_eq!(
            fs::read_dir(previews).unwrap().count(),
            MAX_TEST_REPORT_ARTIFACTS
        );
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn concurrent_test_report_exports_cannot_exceed_retention_cap() {
        let project = temporary_project();
        let editor = Arc::new(EditorSession::open(&project).unwrap());
        let previews = editor.project_root().join(".hycel/editor-previews");
        fs::create_dir_all(&previews).unwrap();
        for index in 0..MAX_TEST_REPORT_ARTIFACTS - 1 {
            fs::write(
                previews.join(format!("test-report-existing-{index}.json")),
                b"prior report",
            )
            .unwrap();
        }
        let report = EditorTestReport {
            tests: vec![EditorTestCase {
                name: "scenario".to_owned(),
                passed: true,
                message: "passed".to_owned(),
            }],
            passed_count: 1,
            failed_count: 0,
        };
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let editor = Arc::clone(&editor);
                let report = report.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    editor.export_test_report(&report)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert_eq!(
            fs::read_dir(previews).unwrap().count(),
            MAX_TEST_REPORT_ARTIFACTS
        );
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn test_report_export_is_bounded_unique_and_does_not_overwrite_project_files() {
        let project = temporary_project();
        let editor = EditorSession::open(&project).unwrap();
        let scene_path = project.join("scenes/first-room.json");
        let source_before = fs::read(&scene_path).unwrap();
        let report = EditorTestReport {
            tests: vec![EditorTestCase {
                name: "echo-flight".to_owned(),
                passed: false,
                message: "the echo did not open the gate".to_owned(),
            }],
            passed_count: 0,
            failed_count: 1,
        };
        let first = editor.export_test_report(&report).unwrap();
        let second = editor.export_test_report(&report).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(editor.project_root().join(".hycel/editor-previews")));
        let artifact: Value = serde_json::from_slice(&fs::read(first).unwrap()).unwrap();
        assert_eq!(artifact["schema_version"], 1);
        assert_eq!(artifact["report"]["tests"][0]["name"], "echo-flight");
        assert_eq!(artifact["report"]["tests"][0]["passed"], false);
        assert_eq!(artifact["report"]["failed_count"], 1);
        assert_eq!(fs::read(scene_path).unwrap(), source_before);
        let mut invalid = report;
        invalid.failed_count = 0;
        assert!(editor.export_test_report(&invalid).is_err());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn editor_exports_authored_scene_svg_without_modifying_source_files() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let project = temporary_directory();
        copy_directory(&source, &project);
        let scene = project.join("scenes/first-room.json");
        let before = fs::read(&scene).unwrap();
        let editor = EditorSession::open(&project).unwrap();
        let first = editor.export_screenshot().unwrap();
        let second = editor.export_screenshot().unwrap();
        assert_ne!(first, second);
        assert!(fs::read_to_string(first).unwrap().contains("<svg"));
        assert!(fs::read_to_string(second).unwrap().contains("<svg"));
        assert_eq!(fs::read(scene).unwrap(), before);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn editor_runs_named_reference_game_diagnostics_without_project_writes() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let project = temporary_directory();
        copy_directory(&source, &project);
        let before = fs::read(project.join("scenes/first-room.json")).unwrap();
        let editor = EditorSession::open(&project).unwrap();
        assert_eq!(editor.run_tests().unwrap(), (5, 0));
        let report = editor.run_tests_detailed().unwrap();
        assert_eq!(report.passed_count, 5);
        assert_eq!(report.failed_count, 0);
        assert_eq!(report.tests.len(), 5);
        assert!(report.tests.iter().all(|test| test.passed));
        assert_eq!(
            fs::read(project.join("scenes/first-room.json")).unwrap(),
            before
        );
        fs::remove_dir_all(project).unwrap();

        let courier =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bellglass-courier");
        let courier_editor = EditorSession::open(&courier).unwrap();
        let courier_report = courier_editor.run_tests_detailed().unwrap();
        assert_eq!(courier_report.passed_count, 6);
        assert_eq!(courier_report.failed_count, 0);
        assert!(
            courier_report
                .tests
                .iter()
                .any(|test| test.name == "echo-flight" && test.passed)
        );
    }

    #[test]
    fn editor_import_panel_uses_paginated_cli_resource_dependencies() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let editor = EditorSession::open(&project).unwrap();
        assert_eq!(editor.resource_count(), 3);
        let animation = editor
            .resources()
            .iter()
            .find(|resource| resource.kind == "animation")
            .unwrap();
        assert_eq!(animation.dependency_count, 2);
        assert!(animation.source.ends_with(".animation.json"));
    }

    #[test]
    fn import_inspector_hashes_source_and_reports_cached_reimport_decisions() {
        use hycel_assets::{ImportRecord, ReimportDecision, ReimportReason, hash_source_file};
        use hycel_project::{ResourceDescriptor, ResourceRegistry};

        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let project = temporary_directory();
        copy_directory(&source, &project);
        let editor = EditorSession::open(&project).unwrap();
        let resource_index = editor
            .resources()
            .iter()
            .position(|resource| resource.kind == "texture")
            .unwrap();
        let resource = &editor.resources()[resource_index];
        let status = editor.inspect_resource_import(resource_index).unwrap();
        assert_eq!(status.source_sha256.len(), 64);
        assert!(status.records.is_empty());

        let mut registry = ResourceRegistry::default();
        registry
            .register("texture", std::iter::empty::<&str>())
            .unwrap();
        let descriptor_path = project.join(&resource.descriptor_file);
        let descriptor = ResourceDescriptor::parse_json_with_registry(
            &fs::read(descriptor_path).unwrap(),
            &resource.descriptor_file,
            &registry,
        )
        .unwrap();
        let source_hash =
            hash_source_file(&project, descriptor.source(), MAX_RESOURCE_SOURCE_BYTES).unwrap();
        let record =
            ImportRecord::new(&descriptor, source_hash, "test-texture-importer-1").unwrap();
        let cache = project.join(record.cache_relative_path());
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("record.json"), record.to_json().unwrap()).unwrap();
        let output_path = cache.join("texture.bin");
        fs::write(&output_path, b"derived texture").unwrap();
        let alternate_record =
            ImportRecord::new(&descriptor, source_hash, "test-texture-importer-2").unwrap();
        let alternate_cache = project.join(alternate_record.cache_relative_path());
        fs::create_dir_all(&alternate_cache).unwrap();
        fs::write(
            alternate_cache.join("record.json"),
            alternate_record.to_json().unwrap(),
        )
        .unwrap();
        fs::write(
            alternate_cache.join("texture.bin"),
            b"alternate derived texture",
        )
        .unwrap();

        let status = editor.inspect_resource_import(resource_index).unwrap();
        assert_eq!(status.records.len(), 2);
        assert!(status.records.iter().all(|record| {
            record.decision_using_recorded_version == ReimportDecision::ReuseCachedOutput
                && record.has_derived_output
        }));
        assert!(
            status
                .records
                .iter()
                .any(|record| { record.importer_version == "test-texture-importer-2" })
        );

        fs::remove_file(output_path).unwrap();
        let status = editor.inspect_resource_import(resource_index).unwrap();
        let missing_output_record = status
            .records
            .iter()
            .find(|record| record.importer_version == "test-texture-importer-1")
            .unwrap();
        assert_eq!(
            missing_output_record.decision_using_recorded_version,
            ReimportDecision::Import(ReimportReason::CachedOutputMissing)
        );

        let source_path = project.join(descriptor.source());
        let mut changed = fs::read(&source_path).unwrap();
        changed.push(0);
        fs::write(source_path, changed).unwrap();
        let status = editor.inspect_resource_import(resource_index).unwrap();
        assert_eq!(
            status.records[0].decision_using_recorded_version,
            ReimportDecision::Import(ReimportReason::SourceChanged)
        );
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn edit_preview_apply_and_discard_round_trip_through_shared_cli_services() {
        let project = temporary_project();
        let scene_path = project.join("scenes/first-room.json");
        let mut document: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
        document["entities"] = json!([{"id":"21000000-0000-4000-8000-000000000001","name":"Player","tags":[],"transform":{"translation_milli":[0,0]},"components":[]}]);
        fs::write(&scene_path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
        let original = fs::read(&scene_path).unwrap();
        let mut editor = EditorSession::open(&project).unwrap();
        assert!(!editor.project_name().is_empty());
        assert_eq!(editor.resource_count(), 0);
        assert_eq!(editor.selected_entity().unwrap().name, "Player");
        editor.stage_move([25, -10]).unwrap();
        assert!(editor.is_dirty());
        let preview = editor.preview().unwrap();
        assert!(!preview.diff.is_empty());
        assert_eq!(fs::read(&scene_path).unwrap(), original);
        let receipt = editor.apply_preview().unwrap();
        assert_ne!(receipt.original_sha256, receipt.candidate_sha256);
        assert!(project.join(receipt.backup_path.unwrap()).is_file());
        assert!(!editor.is_dirty());
        assert_eq!(
            editor.selected_entity().unwrap().translation_milli,
            [25, -10]
        );

        editor.stage_rename("Unsaved Rename".to_owned()).unwrap();
        editor.discard().unwrap();
        assert!(!editor.is_dirty());
        assert_eq!(editor.selected_entity().unwrap().name, "Player");
        editor
            .stage_toggle_entity_tag("courier".to_owned())
            .unwrap();
        assert!(editor.preview().unwrap().diff.contains("courier"));
        editor.apply_preview().unwrap();
        assert_eq!(editor.selected_entity().unwrap().tags, ["courier"]);
        editor
            .stage_toggle_entity_tag("courier".to_owned())
            .unwrap();
        editor.preview().unwrap();
        editor.apply_preview().unwrap();
        assert!(editor.selected_entity().unwrap().tags.is_empty());
        assert!(
            editor
                .stage_toggle_entity_tag("NOT-LOWERCASE".to_owned())
                .is_err()
        );
        let created_id = editor
            .stage_create_entity("Moon Marker".to_owned())
            .unwrap();
        let preview = editor.preview().unwrap();
        assert!(preview.diff.contains("Moon Marker"));
        editor.apply_preview().unwrap();
        assert_eq!(editor.selected_entity().unwrap().id, created_id);
        assert_eq!(editor.current_scene().entities.len(), 2);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn scene_creation_and_rename_are_previewed_revalidated_and_non_overwriting() {
        let project = temporary_project();
        let scene_path = project.join("scenes/first-room.json");
        let mut document: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
        document["entities"] = json!([{"id":"21000000-0000-4000-8000-000000000001","name":"Player","tags":[],"transform":{"translation_milli":[0,0]},"components":[]}]);
        fs::write(&scene_path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
        let mut editor = EditorSession::open(&project).unwrap();

        let discarded_id = editor
            .stage_create_scene("Discarded Room".to_owned())
            .unwrap();
        let discarded_path = project.join(format!("scenes/room-{discarded_id}.json"));
        editor.preview().unwrap();
        editor.discard().unwrap();
        assert!(!discarded_path.exists());
        let original_scene_path = editor.current_scene().relative_path.clone();
        editor
            .stage_rename("Player After Discard".to_owned())
            .unwrap();
        assert_eq!(editor.preview().unwrap().relative_path, original_scene_path);
        editor.apply_preview().unwrap();

        let collision_id = editor.stage_create_scene("Stale Room".to_owned()).unwrap();
        editor.preview().unwrap();
        let collision_path = project.join(format!("scenes/room-{collision_id}.json"));
        let external_scene = serde_json::to_vec(&json!({
            "schema_version": 2,
            "id": "11000000-0000-4000-8000-000000000099",
            "name": "External Scene",
            "entities": []
        }))
        .unwrap();
        fs::write(&collision_path, &external_scene).unwrap();
        assert!(editor.apply_preview().is_err());
        assert_eq!(fs::read(&collision_path).unwrap(), external_scene);
        editor.discard().unwrap();
        assert!(editor.scenes().iter().any(|scene| {
            scene.id == "11000000-0000-4000-8000-000000000099" && scene.name == "External Scene"
        }));

        let scene_id = editor
            .stage_create_scene("Authoring Room".to_owned())
            .unwrap();
        assert!(editor.preview().unwrap().diff.contains("Authoring Room"));
        let relative_path = format!("scenes/room-{scene_id}.json");
        let created_path = project.join(&relative_path);
        assert!(!created_path.exists());
        assert!(editor.apply_preview().unwrap().backup_path.is_none());
        assert_eq!(editor.current_scene().id, scene_id);
        editor
            .stage_rename_scene("Renamed Room".to_owned())
            .unwrap();
        assert_eq!(editor.preview().unwrap().relative_path, relative_path);
        editor.apply_preview().unwrap();
        assert_eq!(editor.current_scene().name, "Renamed Room");
        assert_eq!(editor.scenes().len(), 3);
        editor.check().unwrap();
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn component_and_entity_deletion_are_staged_previewed_and_revalidated() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let project = temporary_directory();
        copy_directory(&source, &project);
        let mut editor = EditorSession::open(&project).unwrap();
        let entity_id = editor.selected_entity().unwrap().id.clone();
        editor.stage_remove_component("hycel.animation").unwrap();
        assert!(editor.preview().unwrap().diff.contains("hycel.animation"));
        editor.apply_preview().unwrap();
        assert!(editor.selected_entity().unwrap().component_types.is_empty());
        assert!(editor.select_entity(&entity_id));
        editor.stage_delete_entity().unwrap();
        assert!(editor.preview().unwrap().diff.contains("Player"));
        editor.apply_preview().unwrap();
        assert!(
            editor.scenes()[0]
                .entities
                .iter()
                .all(|entity| entity.id != entity_id)
        );
        editor.check().unwrap();
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn animation_component_upsert_uses_registered_resource_and_is_transactional() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let project = temporary_directory();
        copy_directory(&source, &project);
        let original_descriptor: Value = serde_json::from_slice(
            &fs::read(project.join("assets/player-run.animation.hycel.json")).unwrap(),
        )
        .unwrap();
        let mut alternate_descriptor = original_descriptor;
        alternate_descriptor["id"] = json!("30000000-0000-4000-8000-000000000004");
        fs::write(
            project.join("assets/z-alternate-run.animation.hycel.json"),
            serde_json::to_vec_pretty(&alternate_descriptor).unwrap(),
        )
        .unwrap();
        let mut editor = EditorSession::open(&project).unwrap();
        let animations = editor
            .animation_resources()
            .map(|resource| resource.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(animations.len(), 2);
        let scene_path = project.join("scenes/first-room.json");
        let original = fs::read(&scene_path).unwrap();
        editor.stage_animation_component_at(1).unwrap();
        let preview = editor.preview().unwrap();
        assert!(preview.diff.contains("hycel.animation"));
        assert!(preview.diff.contains("clip_id"));
        assert!(preview.diff.contains(&animations[1]));
        assert_eq!(fs::read(&scene_path).unwrap(), original);
        editor.apply_preview().unwrap();
        assert!(
            editor
                .selected_entity()
                .unwrap()
                .component_types
                .iter()
                .any(|kind| kind == "hycel.animation")
        );
        assert!(
            fs::read_to_string(&scene_path)
                .unwrap()
                .contains(&animations[1])
        );
        assert!(editor.stage_animation_component_at(2).is_err());
        editor.check().unwrap();
        fs::remove_dir_all(project).unwrap();
    }

    fn copy_directory(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if source_path.is_dir() {
                copy_directory(&source_path, &destination_path);
            } else {
                fs::copy(source_path, destination_path).unwrap();
            }
        }
    }

    fn temporary_directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hycel-editor-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn temporary_project() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hycel-editor-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let output = execute(["new", path.to_str().unwrap(), "--json"].map(Into::into));
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        path
    }
}
