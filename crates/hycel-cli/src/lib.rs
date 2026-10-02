//! Versioned project CLI foundations for Hycel.
//!
//! The library API runs the same command surface as the `hycel` binary and
//! returns captured output/status values, which keeps command behavior testable
//! without spawning a process.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use getrandom::fill as getrandom_fill;
use hycel_assets::AssetDependencyReport;
use hycel_input::{
    AxisBinding, ButtonBinding, FocusLossBehavior, InputBindings, InputControl, KeyCode,
};
use hycel_project::{
    ComponentRegistry, Diagnostic, MAX_DOCUMENT_BYTES, ProjectManifest, ResourceDescriptor,
    ResourceRegistry, SceneDocument, resolve_existing_project_path, validate_project_documents,
    validate_project_root,
};
use serde::Serialize;

const MAX_DIRECTORY_DEPTH: usize = 128;
const MAX_ENTRIES_PER_DIRECTORY: usize = 10_000;
const MAX_PROJECT_DIRECTORIES: usize = 10_000;
const MAX_PROJECT_ENTRIES: usize = 100_000;
const MAX_PROJECT_DOCUMENTS: usize = 10_000;
const MAX_PROJECT_DOCUMENT_BYTES: usize = 256 * 1024 * 1024;

/// Stable command output, suitable for direct testing or a process entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    /// Process status as specified in `docs/cli.md`.
    pub exit_code: i32,
    /// Human-readable output or a single JSON envelope.
    pub stdout: String,
    /// Human-readable failure diagnostics; empty for JSON mode.
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize)]
struct OutputDiagnostic {
    code: &'static str,
    file: Option<String>,
    path: String,
    message: String,
}

impl From<Diagnostic> for OutputDiagnostic {
    fn from(diagnostic: Diagnostic) -> Self {
        Self {
            code: diagnostic.code,
            file: diagnostic.file,
            path: diagnostic.path,
            message: diagnostic.message,
        }
    }
}

#[derive(Serialize)]
struct Envelope<T: Serialize> {
    schema_version: u32,
    command: String,
    ok: bool,
    result: Option<T>,
    diagnostics: Vec<OutputDiagnostic>,
}

#[derive(Debug)]
enum Command {
    Help,
    Version,
    New { path: PathBuf, name: Option<String> },
    Check { path: PathBuf },
    Inspect { path: PathBuf },
}

struct ParsedArgs {
    command: Command,
    json: bool,
}

#[derive(Serialize)]
struct HelpResult {
    usage: &'static str,
    commands: Vec<&'static str>,
    json_output: &'static str,
    exit_codes: Vec<ExitCodeInfo>,
}

#[derive(Serialize)]
struct ExitCodeInfo {
    code: i32,
    meaning: &'static str,
}

#[derive(Serialize)]
struct VersionResult {
    version: &'static str,
}

#[derive(Serialize)]
struct NewResult {
    path: String,
    project_id: String,
    project_name: String,
    initial_scene_id: String,
}

#[derive(Serialize)]
struct CheckResult {
    format_version: u32,
    project_id: String,
    project_name: String,
    scene_count: usize,
    resource_count: usize,
}

#[derive(Serialize)]
struct InspectResult {
    format_version: u32,
    project_id: String,
    project_name: String,
    default_profile: String,
    profiles: Vec<BuildProfileInfo>,
    scenes: Vec<SceneInfo>,
    resources: Vec<ResourceInfo>,
    dependencies: AssetDependencyReport,
}

#[derive(Serialize)]
struct BuildProfileInfo {
    name: String,
    optimization: u8,
    debug_info: bool,
}

#[derive(Serialize)]
struct SceneInfo {
    id: String,
    name: String,
    file: String,
    entity_count: usize,
    component_count: usize,
}

#[derive(Serialize)]
struct ResourceInfo {
    id: String,
    kind: String,
    descriptor_file: String,
    source: String,
}

#[derive(Serialize)]
struct NewScene {
    schema_version: u32,
    id: String,
    name: String,
    entities: Vec<serde_json::Value>,
}

/// Executes the command from its arguments (excluding the executable name).
///
/// Supported forms:
///
/// - `hycel new <path> [--name <display-name>] [--json]`
/// - `hycel check [<project-path>] [--json]`
/// - `hycel inspect [<project-path>] [--json]`
/// - `hycel --help` / `hycel --version`
///
/// # Errors
///
/// Command failures are represented by `CliOutput.exit_code`, `stdout`, and
/// `stderr`; this function does not terminate the calling process.
#[must_use]
pub fn execute(arguments: impl IntoIterator<Item = OsString>) -> CliOutput {
    let args = arguments.into_iter().collect::<Vec<_>>();
    let json = args.iter().any(|argument| argument == "--json");
    match parse_arguments(args) {
        Ok(parsed) => execute_command(parsed.command, parsed.json),
        Err((command, message)) => failure(
            &command,
            2,
            vec![OutputDiagnostic {
                code: "HYCEL-CLI-001",
                file: None,
                path: "arguments".to_owned(),
                message,
            }],
            json,
        ),
    }
}

fn parse_arguments(mut arguments: Vec<OsString>) -> Result<ParsedArgs, (String, String)> {
    let json = arguments.iter().any(|argument| argument == "--json");
    arguments.retain(|argument| argument != "--json");
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        return Ok(ParsedArgs {
            command: Command::Help,
            json,
        });
    }
    if arguments
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        return Ok(ParsedArgs {
            command: Command::Version,
            json,
        });
    }
    if arguments.is_empty() {
        return Ok(ParsedArgs {
            command: Command::Help,
            json,
        });
    }
    let command = arguments[0]
        .to_str()
        .ok_or_else(|| ("usage".to_owned(), "command name must be UTF-8".to_owned()))?;
    let rest = &arguments[1..];
    let parsed = match command {
        "new" => parse_new_arguments(rest)?,
        "check" => Command::Check {
            path: parse_project_path("check", rest)?,
        },
        "inspect" => Command::Inspect {
            path: parse_project_path("inspect", rest)?,
        },
        _ => return Err((command.to_owned(), format!("unknown command: {command}"))),
    };
    Ok(ParsedArgs {
        command: parsed,
        json,
    })
}

fn parse_new_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut path = None;
    let mut name = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--name" {
            if name.is_some() || index + 1 >= arguments.len() {
                return Err((
                    "new".to_owned(),
                    "--name requires one display-name value and may be used only once".to_owned(),
                ));
            }
            name = Some(
                arguments[index + 1]
                    .to_str()
                    .ok_or_else(|| {
                        (
                            "new".to_owned(),
                            "project display name must be UTF-8".to_owned(),
                        )
                    })?
                    .to_owned(),
            );
            index += 2;
        } else if arguments[index].to_string_lossy().starts_with('-') {
            return Err((
                "new".to_owned(),
                format!("unknown option: {}", arguments[index].to_string_lossy()),
            ));
        } else if path.replace(PathBuf::from(&arguments[index])).is_some() {
            return Err((
                "new".to_owned(),
                "new accepts exactly one destination path".to_owned(),
            ));
        } else {
            index += 1;
        }
    }
    let path = path.ok_or_else(|| {
        (
            "new".to_owned(),
            "new requires a destination path".to_owned(),
        )
    })?;
    Ok(Command::New { path, name })
}

fn parse_project_path(command: &str, arguments: &[OsString]) -> Result<PathBuf, (String, String)> {
    if arguments
        .first()
        .is_some_and(|argument| argument.to_string_lossy().starts_with('-'))
    {
        return Err((
            command.to_owned(),
            format!("unknown option: {}", arguments[0].to_string_lossy()),
        ));
    }
    if arguments.len() > 1 {
        return Err((
            command.to_owned(),
            format!("{command} accepts at most one project path"),
        ));
    }
    Ok(arguments
        .first()
        .map_or_else(|| PathBuf::from("."), PathBuf::from))
}

fn execute_command(command: Command, json: bool) -> CliOutput {
    match command {
        Command::Help => {
            let result = HelpResult {
                usage: "hycel <new|check|inspect> [arguments] [--json]",
                commands: vec![
                    "new <path> [--name <display-name>]",
                    "check [<project-path>]",
                    "inspect [<project-path>]",
                ],
                json_output: "Version-1 envelope: schema_version, command, ok, result, diagnostics.",
                exit_codes: vec![
                    ExitCodeInfo {
                        code: 0,
                        meaning: "success",
                    },
                    ExitCodeInfo {
                        code: 1,
                        meaning: "project validation failed",
                    },
                    ExitCodeInfo {
                        code: 2,
                        meaning: "invalid command or arguments",
                    },
                    ExitCodeInfo {
                        code: 3,
                        meaning: "filesystem or command execution failure",
                    },
                ],
            };
            success(
                "help",
                result,
                json,
                "Hycel project tools\nRun `hycel new <path>`, `hycel check`, or `hycel inspect`.\n",
            )
        }
        Command::Version => {
            let result = VersionResult {
                version: env!("CARGO_PKG_VERSION"),
            };
            let human = format!("hycel {}\n", result.version);
            success("version", result, json, &human)
        }
        Command::New { path, name } => create_project(&path, name.as_deref(), json),
        Command::Check { path } => match load_project(&path) {
            Ok(project) => {
                let result = CheckResult {
                    format_version: project.manifest.format_version(),
                    project_id: project.manifest.project_id().to_owned(),
                    project_name: project.manifest.project_name().to_owned(),
                    scene_count: project.scenes.len(),
                    resource_count: project.resources.len(),
                };
                let human = format!(
                    "Project {:?} is valid ({} scene(s), {} resource(s)).\n",
                    result.project_name, result.scene_count, result.resource_count
                );
                success("check", result, json, &human)
            }
            Err(diagnostics) => failure("check", 1, diagnostics, json),
        },
        Command::Inspect { path } => inspect_project(&path, json),
    }
}

struct LoadedProject {
    manifest: ProjectManifest,
    scenes: Vec<(String, SceneDocument)>,
    resources: Vec<(String, ResourceDescriptor)>,
    dependencies: AssetDependencyReport,
}

type ProjectDocuments = (
    Vec<(String, SceneDocument)>,
    Vec<(String, ResourceDescriptor)>,
);

fn inspect_project(path: &Path, json: bool) -> CliOutput {
    match load_project(path) {
        Ok(project) => {
            let profiles = project
                .manifest
                .build_profiles()
                .map(|(name, optimization, debug_info)| BuildProfileInfo {
                    name: name.to_owned(),
                    optimization,
                    debug_info,
                })
                .collect();
            let scenes = project
                .scenes
                .iter()
                .map(|(file, scene)| SceneInfo {
                    id: scene.id().to_owned(),
                    name: scene.name().to_owned(),
                    file: file.clone(),
                    entity_count: scene.entities().len(),
                    component_count: scene
                        .entities()
                        .iter()
                        .map(|entity| entity.components().len())
                        .sum(),
                })
                .collect::<Vec<_>>();
            let resources = project
                .resources
                .iter()
                .map(|(file, resource)| ResourceInfo {
                    id: resource.id().to_owned(),
                    kind: resource.kind().to_owned(),
                    descriptor_file: file.clone(),
                    source: resource.source().to_owned(),
                })
                .collect::<Vec<_>>();
            let result = InspectResult {
                format_version: project.manifest.format_version(),
                project_id: project.manifest.project_id().to_owned(),
                project_name: project.manifest.project_name().to_owned(),
                default_profile: project.manifest.default_profile().to_owned(),
                profiles,
                scenes,
                resources,
                dependencies: project.dependencies,
            };
            let human = human_inspect(&result);
            success("inspect", result, json, &human)
        }
        Err(diagnostics) => failure("inspect", 1, diagnostics, json),
    }
}

fn load_project(path: &Path) -> Result<LoadedProject, Vec<OutputDiagnostic>> {
    let manifest = validate_project_root(path).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(OutputDiagnostic::from)
            .collect::<Vec<_>>()
    })?;
    let root = path.canonicalize().map_err(|error| {
        vec![cli_diagnostic(
            "HYCEL-CLI-101",
            None,
            "project_path",
            format!("cannot resolve project root: {error}"),
        )]
    })?;
    let mut diagnostics = Vec::new();
    let mut directory_scan = DirectoryScan::default();
    let scene_files = collect_files(
        &root,
        "scenes",
        |name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        },
        &mut directory_scan,
    );
    let resource_files = collect_files(
        &root,
        "assets",
        |name| name.to_ascii_lowercase().ends_with(".hycel.json"),
        &mut directory_scan,
    );
    diagnostics.extend(directory_scan.diagnostics);

    let component_registry = ComponentRegistry::default();
    let (scenes, resources) = parse_project_documents(
        &root,
        scene_files,
        resource_files,
        &component_registry,
        diagnostics,
    )?;
    let dependencies = AssetDependencyReport::build(&scenes, &resources, &component_registry)
        .map_err(|error| {
            vec![cli_diagnostic(
                "HYCEL-CLI-102",
                None,
                "dependencies",
                error.to_string(),
            )]
        })?;
    Ok(LoadedProject {
        manifest,
        scenes,
        resources,
        dependencies,
    })
}

fn parse_project_documents(
    root: &Path,
    scene_files: Vec<String>,
    resource_files: Vec<String>,
    component_registry: &ComponentRegistry,
    mut diagnostics: Vec<OutputDiagnostic>,
) -> Result<ProjectDocuments, Vec<OutputDiagnostic>> {
    if scene_files.len().saturating_add(resource_files.len()) > MAX_PROJECT_DOCUMENTS {
        diagnostics.push(cli_diagnostic(
            "HYCEL-CLI-110",
            None,
            "project",
            format!("project exceeds the {MAX_PROJECT_DOCUMENTS}-document discovery limit"),
        ));
        return Err(diagnostics);
    }
    let resource_registry = ResourceRegistry::default();
    let mut total_bytes = 0_usize;
    let mut scenes = Vec::new();
    for file in scene_files {
        match read_project_document(root, &file, &mut total_bytes) {
            Ok(bytes) => {
                match SceneDocument::parse_json_with_registry(&bytes, &file, component_registry) {
                    Ok(scene) => scenes.push((file, scene)),
                    Err(errors) => {
                        diagnostics.extend(errors.into_iter().map(OutputDiagnostic::from));
                    }
                }
            }
            Err(error) => {
                let aggregate_limit = error.code == "HYCEL-CLI-112";
                diagnostics.push(error);
                if aggregate_limit {
                    return Err(diagnostics);
                }
            }
        }
    }
    let mut resources = Vec::new();
    for file in resource_files {
        match read_project_document(root, &file, &mut total_bytes) {
            Ok(bytes) => match ResourceDescriptor::parse_json_with_registry(
                &bytes,
                &file,
                &resource_registry,
            ) {
                Ok(resource) => resources.push((file, resource)),
                Err(errors) => diagnostics.extend(errors.into_iter().map(OutputDiagnostic::from)),
            },
            Err(error) => {
                let aggregate_limit = error.code == "HYCEL-CLI-112";
                diagnostics.push(error);
                if aggregate_limit {
                    return Err(diagnostics);
                }
            }
        }
    }
    if let Err(errors) = validate_project_documents(&scenes, &resources, component_registry) {
        diagnostics.extend(errors.into_iter().map(OutputDiagnostic::from));
    }
    if diagnostics.is_empty() {
        Ok((scenes, resources))
    } else {
        Err(diagnostics)
    }
}

fn collect_files(
    root: &Path,
    directory: &str,
    matches: impl Fn(&str) -> bool + Copy,
    state: &mut DirectoryScan,
) -> Vec<String> {
    state.visited_directories.clear();
    let first_new_file = state.files.len();
    walk_project_tree(root, directory, 0, matches, state);
    let mut files = state.files.split_off(first_new_file);
    files.sort();
    files
}

#[derive(Default)]
struct DirectoryScan {
    visited_directories: BTreeSet<PathBuf>,
    entries_visited: usize,
    directories_visited: usize,
    files: Vec<String>,
    diagnostics: Vec<OutputDiagnostic>,
}

fn walk_project_tree(
    root: &Path,
    relative_directory: &str,
    depth: usize,
    matches: impl Fn(&str) -> bool + Copy,
    state: &mut DirectoryScan,
) {
    if depth > MAX_DIRECTORY_DEPTH {
        state.diagnostics.push(cli_diagnostic(
            "HYCEL-CLI-110",
            Some(relative_directory),
            "$",
            format!("project directory nesting exceeds {MAX_DIRECTORY_DEPTH} levels"),
        ));
        return;
    }
    let directory = match resolve_existing_project_path(root, relative_directory) {
        Ok(path) => path,
        Err(error) => {
            state.diagnostics.push(OutputDiagnostic::from(error));
            return;
        }
    };
    if state.visited_directories.contains(&directory) {
        return;
    }
    if state.directories_visited >= MAX_PROJECT_DIRECTORIES {
        state.diagnostics.push(cli_diagnostic(
            "HYCEL-CLI-110",
            Some(relative_directory),
            "$",
            format!("project exceeds the {MAX_PROJECT_DIRECTORIES}-directory scan limit"),
        ));
        return;
    }
    state.visited_directories.insert(directory.clone());
    state.directories_visited += 1;
    let remaining_entries = MAX_PROJECT_ENTRIES.saturating_sub(state.entries_visited);
    if remaining_entries == 0 {
        state.diagnostics.push(cli_diagnostic(
            "HYCEL-CLI-110",
            Some(relative_directory),
            "$",
            format!("project exceeds the {MAX_PROJECT_ENTRIES}-entry scan limit"),
        ));
        return;
    }
    let entry_limit = MAX_ENTRIES_PER_DIRECTORY.min(remaining_entries);
    let global_limit = entry_limit == remaining_entries;
    let Some(entries) = read_sorted_entries(
        &directory,
        relative_directory,
        entry_limit,
        global_limit,
        state,
    ) else {
        return;
    };
    for entry in &entries {
        walk_project_entry(root, relative_directory, depth, entry, matches, state);
    }
}

fn read_sorted_entries(
    directory: &Path,
    relative_directory: &str,
    entry_limit: usize,
    global_limit: bool,
    state: &mut DirectoryScan,
) -> Option<Vec<fs::DirEntry>> {
    let read_dir = fs::read_dir(directory)
        .map_err(|error| {
            state.diagnostics.push(cli_diagnostic(
                "HYCEL-CLI-103",
                Some(relative_directory),
                "$",
                format!("cannot enumerate project directory: {error}"),
            ));
        })
        .ok()?;
    let mut entries = Vec::new();
    for entry in read_dir.take(entry_limit + 1) {
        match entry {
            Ok(entry) if entries.len() < entry_limit => {
                state.entries_visited += 1;
                entries.push(entry);
            }
            Ok(_) => {
                state.entries_visited += 1;
                let message = if global_limit {
                    format!("project exceeds the {MAX_PROJECT_ENTRIES}-entry scan limit")
                } else {
                    format!("directory exceeds the {MAX_ENTRIES_PER_DIRECTORY}-entry scan limit")
                };
                state.diagnostics.push(cli_diagnostic(
                    "HYCEL-CLI-110",
                    Some(relative_directory),
                    "$",
                    message,
                ));
                return None;
            }
            Err(error) => {
                state.diagnostics.push(cli_diagnostic(
                    "HYCEL-CLI-103",
                    Some(relative_directory),
                    "$",
                    format!("cannot read project directory entry: {error}"),
                ));
                return None;
            }
        }
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Some(entries)
}

fn walk_project_entry(
    root: &Path,
    relative_directory: &str,
    depth: usize,
    entry: &fs::DirEntry,
    matches: impl Fn(&str) -> bool + Copy,
    state: &mut DirectoryScan,
) {
    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
        state.diagnostics.push(cli_diagnostic(
            "HYCEL-CLI-104",
            Some(relative_directory),
            "$",
            "project directory contains a non-UTF-8 entry name",
        ));
        return;
    };
    let child = format!("{relative_directory}/{name}");
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(error) => {
            state.diagnostics.push(cli_diagnostic(
                "HYCEL-CLI-103",
                Some(&child),
                "$",
                format!("cannot inspect project path: {error}"),
            ));
            return;
        }
    };
    let resolved = if file_type.is_symlink() {
        match resolve_existing_project_path(root, &child) {
            Ok(path) => path,
            Err(error) => {
                state.diagnostics.push(OutputDiagnostic::from(error));
                return;
            }
        }
    } else {
        entry.path()
    };
    let metadata = match fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(error) => {
            state.diagnostics.push(cli_diagnostic(
                "HYCEL-CLI-103",
                Some(&child),
                "$",
                format!("cannot inspect project path: {error}"),
            ));
            return;
        }
    };
    if metadata.is_dir() {
        walk_project_tree(root, &child, depth + 1, matches, state);
    } else if metadata.is_file() && matches(&name) {
        if let Err(error) = hycel_project::validate_relative_project_path(&child) {
            state.diagnostics.push(OutputDiagnostic::from(error));
        } else {
            state.files.push(child);
        }
    }
}

fn read_project_document(
    root: &Path,
    relative_path: &str,
    total_bytes: &mut usize,
) -> Result<Vec<u8>, OutputDiagnostic> {
    let remaining = MAX_PROJECT_DOCUMENT_BYTES.saturating_sub(*total_bytes);
    if remaining == 0 {
        return Err(cli_diagnostic(
            "HYCEL-CLI-112",
            Some(relative_path),
            "$",
            format!(
                "project documents exceed the {MAX_PROJECT_DOCUMENT_BYTES}-byte aggregate limit"
            ),
        ));
    }
    let path =
        resolve_existing_project_path(root, relative_path).map_err(OutputDiagnostic::from)?;
    let file = File::open(path).map_err(|error| {
        cli_diagnostic(
            "HYCEL-CLI-105",
            Some(relative_path),
            "$",
            format!("cannot open project document: {error}"),
        )
    })?;
    let limit = MAX_DOCUMENT_BYTES.min(remaining);
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            cli_diagnostic(
                "HYCEL-CLI-105",
                Some(relative_path),
                "$",
                format!("cannot read project document: {error}"),
            )
        })?;
    if bytes.len() > limit {
        let (code, message) = if remaining < MAX_DOCUMENT_BYTES {
            (
                "HYCEL-CLI-112",
                format!(
                    "project documents exceed the {MAX_PROJECT_DOCUMENT_BYTES}-byte aggregate limit"
                ),
            )
        } else {
            (
                "HYCEL-DOCUMENT-001",
                format!("document exceeds the {MAX_DOCUMENT_BYTES}-byte limit"),
            )
        };
        return Err(cli_diagnostic(code, Some(relative_path), "$", message));
    }
    let new_total = total_bytes.saturating_add(bytes.len());
    *total_bytes = new_total;
    Ok(bytes)
}

struct PreparedProject {
    project_id: String,
    scene_id: String,
    manifest_bytes: Vec<u8>,
    input_bytes: Vec<u8>,
    scene_bytes: Vec<u8>,
}

fn prepare_project(name: &str) -> Result<PreparedProject, (i32, OutputDiagnostic)> {
    let project_id = new_uuid_v4().map_err(|error| {
        (
            3,
            cli_diagnostic(
                "HYCEL-CLI-106",
                None,
                "project.id",
                format!("cannot obtain random project identity: {error}"),
            ),
        )
    })?;
    let scene_id = new_uuid_v4().map_err(|error| {
        (
            3,
            cli_diagnostic(
                "HYCEL-CLI-106",
                None,
                "scene.id",
                format!("cannot obtain random scene identity: {error}"),
            ),
        )
    })?;
    let manifest = ProjectManifest::new(&project_id, name)
        .map_err(|error| (2, OutputDiagnostic::from(error)))?;
    let manifest_bytes = manifest
        .to_toml()
        .map_err(|error| {
            (
                3,
                cli_diagnostic(
                    "HYCEL-CLI-107",
                    None,
                    "hycel.toml",
                    format!("cannot serialize project manifest: {error}"),
                ),
            )
        })?
        .into_bytes();
    let input_bytes = starter_input_bytes()?;
    let scene = NewScene {
        schema_version: 2,
        id: scene_id.clone(),
        name: "First Room".to_owned(),
        entities: Vec::new(),
    };
    let scene_bytes = serde_json::to_vec_pretty(&scene)
        .map_err(|error| {
            (
                3,
                cli_diagnostic(
                    "HYCEL-CLI-107",
                    None,
                    "scenes/first-room.json",
                    format!("cannot serialize starter scene: {error}"),
                ),
            )
        })?
        .into_iter()
        .chain(std::iter::once(b'\n'))
        .collect();
    Ok(PreparedProject {
        project_id,
        scene_id,
        manifest_bytes,
        input_bytes,
        scene_bytes,
    })
}

fn starter_input_bytes() -> Result<Vec<u8>, (i32, OutputDiagnostic)> {
    let input_bindings = InputBindings::new(
        FocusLossBehavior::ReleaseAll,
        vec![ButtonBinding::new(
            1,
            vec![InputControl::Key {
                code: KeyCode::Space,
            }],
        )],
        vec![AxisBinding::new(
            0,
            vec![
                InputControl::Key {
                    code: KeyCode::KeyA,
                },
                InputControl::Key {
                    code: KeyCode::ArrowLeft,
                },
            ],
            vec![
                InputControl::Key {
                    code: KeyCode::KeyD,
                },
                InputControl::Key {
                    code: KeyCode::ArrowRight,
                },
            ],
        )],
    )
    .expect("built-in starter input bindings are valid");
    input_bindings
        .to_json()
        .map(String::into_bytes)
        .map_err(|error| {
            (
                3,
                cli_diagnostic(
                    "HYCEL-CLI-107",
                    None,
                    "input.json",
                    format!("cannot serialize starter input bindings: {error}"),
                ),
            )
        })
}

fn create_project(path: &Path, requested_name: Option<&str>, json: bool) -> CliOutput {
    if path.to_str().is_none() {
        return failure(
            "new",
            2,
            vec![cli_diagnostic(
                "HYCEL-CLI-001",
                None,
                "path",
                "destination path must be valid UTF-8",
            )],
            json,
        );
    }
    let name = match requested_name {
        Some(name) => name.to_owned(),
        None => match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name.to_owned(),
            None => {
                return failure(
                    "new",
                    2,
                    vec![cli_diagnostic(
                        "HYCEL-CLI-001",
                        None,
                        "path",
                        "supply --name when destination has no UTF-8 final path component",
                    )],
                    json,
                );
            }
        },
    };
    let prepared = match prepare_project(&name) {
        Ok(prepared) => prepared,
        Err((exit_code, diagnostic)) => {
            return failure("new", exit_code, vec![diagnostic], json);
        }
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Err(error) = fs::create_dir_all(parent) {
        return failure(
            "new",
            3,
            vec![cli_diagnostic(
                "HYCEL-CLI-108",
                None,
                "path",
                format!("cannot create destination parent: {error}"),
            )],
            json,
        );
    }
    if let Err(error) = fs::create_dir(path) {
        return failure(
            "new",
            3,
            vec![cli_diagnostic(
                "HYCEL-CLI-108",
                path.to_str(),
                "path",
                format!(
                    "cannot create destination directory (existing data is never overwritten): {error}"
                ),
            )],
            json,
        );
    }
    for directory in ["src", "assets", "scenes"] {
        if let Err(error) = fs::create_dir(path.join(directory)) {
            return incomplete_project(path, "", &error, json);
        }
    }
    if let Err(error) = write_new_file(&path.join("hycel.toml"), &prepared.manifest_bytes) {
        return incomplete_project(path, "hycel.toml", &error, json);
    }
    if let Err(error) = write_new_file(&path.join("input.json"), &prepared.input_bytes) {
        return incomplete_project(path, "input.json", &error, json);
    }
    if let Err(error) = write_new_file(&path.join("scenes/first-room.json"), &prepared.scene_bytes)
    {
        return incomplete_project(path, "scenes/first-room.json", &error, json);
    }
    if let Err(diagnostics) = load_project(path) {
        return failure("new", 3, diagnostics, json);
    }
    let result = NewResult {
        path: path.to_str().unwrap_or_default().to_owned(),
        project_id: prepared.project_id,
        project_name: name,
        initial_scene_id: prepared.scene_id,
    };
    let human = format!(
        "Created project {:?} at {}\n",
        result.project_name, result.path
    );
    success("new", result, json, &human)
}

fn incomplete_project(path: &Path, file: &str, error: &io::Error, json: bool) -> CliOutput {
    failure(
        "new",
        3,
        vec![cli_diagnostic(
            "HYCEL-CLI-109",
            (!file.is_empty()).then_some(file),
            "write",
            format!(
                "project creation stopped: {error}; partial project was preserved at {} and was not automatically removed",
                path.display()
            ),
        )],
        json,
    )
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn new_uuid_v4() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom_fill(&mut bytes)?;
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

fn human_inspect(result: &InspectResult) -> String {
    let mut output = format!(
        "Project: {} ({})\nDefault profile: {}\nScenes: {}\n",
        result.project_name,
        result.project_id,
        result.default_profile,
        result.scenes.len()
    );
    for scene in &result.scenes {
        let _ = writeln!(
            output,
            "  {} — {:?}: {} entities, {} components",
            scene.file, scene.name, scene.entity_count, scene.component_count
        );
    }
    let _ = writeln!(output, "Resources: {}", result.resources.len());
    for resource in &result.resources {
        let _ = writeln!(
            output,
            "  {} — {} ({})",
            resource.descriptor_file, resource.id, resource.kind
        );
    }
    output
}

fn success<T: Serialize>(command: &str, result: T, json: bool, human: &str) -> CliOutput {
    if json {
        CliOutput {
            exit_code: 0,
            stdout: serialize_envelope(command, true, Some(result), Vec::new()),
            stderr: String::new(),
        }
    } else {
        CliOutput {
            exit_code: 0,
            stdout: human.to_owned(),
            stderr: String::new(),
        }
    }
}

fn failure(
    command: &str,
    exit_code: i32,
    diagnostics: Vec<OutputDiagnostic>,
    json: bool,
) -> CliOutput {
    if json {
        CliOutput {
            exit_code,
            stdout: serialize_envelope::<serde_json::Value>(command, false, None, diagnostics),
            stderr: String::new(),
        }
    } else {
        let stderr = diagnostics
            .iter()
            .map(|diagnostic| {
                if let Some(file) = &diagnostic.file {
                    format!(
                        "{file}:{}: {} ({})\n",
                        diagnostic.path, diagnostic.message, diagnostic.code
                    )
                } else {
                    format!(
                        "{}: {} ({})\n",
                        diagnostic.message, diagnostic.path, diagnostic.code
                    )
                }
            })
            .collect();
        CliOutput {
            exit_code,
            stdout: String::new(),
            stderr,
        }
    }
}

fn serialize_envelope<T: Serialize>(
    command: &str,
    ok: bool,
    result: Option<T>,
    diagnostics: Vec<OutputDiagnostic>,
) -> String {
    let envelope = Envelope {
        schema_version: 1,
        command: command.to_owned(),
        ok,
        result,
        diagnostics,
    };
    match serde_json::to_string(&envelope) {
        Ok(mut json) => {
            json.push('\n');
            json
        }
        Err(_) => "{\"schema_version\":1,\"command\":\"internal\",\"ok\":false,\"result\":null,\"diagnostics\":[{\"code\":\"HYCEL-CLI-199\",\"file\":null,\"path\":\"output\",\"message\":\"could not serialize command output\"}]}\n".to_owned(),
    }
}

fn cli_diagnostic(
    code: &'static str,
    file: Option<&str>,
    path: &str,
    message: impl Into<String>,
) -> OutputDiagnostic {
    OutputDiagnostic {
        code,
        file: file.map(str::to_owned),
        path: path.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hycel_input::InputBindings;
    use serde_json::Value;

    use super::execute;

    #[test]
    fn new_check_inspect_round_trip_is_stable() {
        let directory = test_directory("cli-round-trip");
        let project = directory.join("my-platformer");
        let created = execute(args([
            "new",
            project.to_str().unwrap(),
            "--name",
            "A \"quoted\" Game",
            "--json",
        ]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        let new_envelope: Value = serde_json::from_str(&created.stdout).unwrap();
        assert_eq!(new_envelope["schema_version"], 1);
        assert_eq!(new_envelope["command"], "new");
        assert_eq!(new_envelope["ok"], true);
        assert_eq!(new_envelope["result"]["project_name"], "A \"quoted\" Game");
        for field in ["project_id", "initial_scene_id"] {
            let id = new_envelope["result"][field].as_str().unwrap();
            assert_eq!(id.len(), 36);
            assert_eq!(id.as_bytes()[14], b'4');
            assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        }

        let input_bytes = fs::read(project.join("input.json")).unwrap();
        let input = InputBindings::parse_json(&input_bytes).unwrap();
        assert_eq!(input.schema_version(), 1);
        assert_eq!(input.buttons().len(), 1);
        assert_eq!(input.axes().len(), 1);

        let check = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(check.exit_code, 0, "{}", check.stdout);
        let first_inspection = execute(args(["inspect", project.to_str().unwrap(), "--json"]));
        let second_inspection = execute(args(["inspect", project.to_str().unwrap(), "--json"]));
        assert_eq!(first_inspection.exit_code, 0);
        assert_eq!(first_inspection.stdout, second_inspection.stdout);
        let inspected: Value = serde_json::from_str(&first_inspection.stdout).unwrap();
        assert_eq!(inspected["result"]["scenes"].as_array().unwrap().len(), 1);
        assert_eq!(inspected["result"]["scenes"][0]["entity_count"], 0);
        assert_eq!(
            inspected["result"]["resources"].as_array().unwrap().len(),
            0
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn check_reports_invalid_input_bindings_with_stable_diagnostics() {
        let directory = test_directory("cli-invalid-input");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        fs::write(
            project.join("input.json"),
            br#"{"schema_version":9,"focus_loss":"release_all","buttons":[],"axes":[]}"#,
        )
        .unwrap();
        let output = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 1);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert!(
            envelope["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| {
                    diagnostic["code"] == "HYCEL-INPUT-003" && diagnostic["file"] == "input.json"
                })
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn check_reports_invalid_documents_and_uses_stable_exit_code() {
        let directory = test_directory("cli-invalid");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        fs::write(
            project.join("scenes/broken.json"),
            b"{\"schema_version\": 9}",
        )
        .unwrap();

        let output = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 1);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["diagnostics"][0]["code"], "HYCEL-SCENE-001");
        assert!(
            envelope["diagnostics"][0]["file"]
                .as_str()
                .unwrap()
                .ends_with("broken.json")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn document_reads_stop_at_the_remaining_aggregate_budget() {
        let directory = test_directory("cli-aggregate-budget");
        let root = directory.join("project");
        fs::create_dir_all(root.join("scenes")).unwrap();
        fs::write(root.join("scenes/next.json"), b"four").unwrap();
        let mut total_bytes = super::MAX_PROJECT_DOCUMENT_BYTES - 2;
        let error =
            super::read_project_document(&root, "scenes/next.json", &mut total_bytes).unwrap_err();
        assert_eq!(error.code, "HYCEL-CLI-112");
        assert_eq!(total_bytes, super::MAX_PROJECT_DOCUMENT_BYTES - 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn check_rejects_unregistered_resources_instead_of_assuming_a_schema() {
        let directory = test_directory("cli-unregistered-resource");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        fs::write(
            project.join("assets/player.png.hycel.json"),
            br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"texture","source":"assets/player.png","import":{}}"#,
        )
        .unwrap();

        let output = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 1);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert!(
            envelope["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "HYCEL-RESOURCE-006")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn check_rejects_oversized_documents_before_json_parsing() {
        let directory = test_directory("cli-oversized");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        let oversized = vec![b' '; hycel_project::MAX_DOCUMENT_BYTES + 1];
        fs::write(project.join("scenes/oversized.json"), oversized).unwrap();

        let output = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 1);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert!(
            envelope["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "HYCEL-DOCUMENT-001")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn new_never_overwrites_existing_directory_or_files() {
        let directory = test_directory("cli-no-overwrite");
        let project = directory.join("existing");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("user.txt"), b"keep me").unwrap();
        let output = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(output.exit_code, 3);
        assert_eq!(fs::read(project.join("user.txt")).unwrap(), b"keep me");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn usage_errors_have_stable_exit_code_and_json_envelope() {
        let output = execute(args(["new", "--json"]));
        assert_eq!(output.exit_code, 2);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["command"], "new");
        assert_eq!(envelope["diagnostics"][0]["code"], "HYCEL-CLI-001");
    }

    #[cfg(unix)]
    #[test]
    fn check_rejects_symlink_escape_during_document_discovery() {
        use std::os::unix::fs::symlink;

        let directory = test_directory("cli-symlink");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        let external = directory.join("external.json");
        fs::write(&external, b"{}").unwrap();
        symlink(&external, project.join("scenes/escape.json")).unwrap();
        let output = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 1);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert!(
            envelope["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == "HYCEL-PATH-004")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    fn args<const N: usize>(values: [&str; N]) -> impl Iterator<Item = std::ffi::OsString> {
        values.into_iter().map(std::ffi::OsString::from)
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
