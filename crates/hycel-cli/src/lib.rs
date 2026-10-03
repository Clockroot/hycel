//! Versioned project CLI foundations for Hycel.
//!
//! The library API runs the same command surface as the `hycel` binary and
//! returns captured output/status values, which keeps command behavior testable
//! without spawning a process.

mod bundle;

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use getrandom::fill as getrandom_fill;
use hycel_assets::AssetDependencyReport;
use hycel_input::{
    AxisBinding, ButtonBinding, FocusLossBehavior, InputBindings, InputControl, KeyCode,
};
use hycel_project::{
    AnimationClipDocument, ComponentRegistry, Diagnostic, MAX_DOCUMENT_BYTES, ProjectManifest,
    ResourceDescriptor, ResourceEditOperation, ResourceRegistry, SceneDocument, SceneEditOperation,
    resolve_existing_project_path, validate_project_documents, validate_project_root,
};
use serde::Serialize;

const MAX_DIRECTORY_DEPTH: usize = 128;
const MAX_ENTRIES_PER_DIRECTORY: usize = 10_000;
const MAX_PROJECT_DIRECTORIES: usize = 10_000;
const MAX_PROJECT_ENTRIES: usize = 100_000;
const MAX_PROJECT_DOCUMENTS: usize = 10_000;
const MAX_PROJECT_DOCUMENT_BYTES: usize = 256 * 1024 * 1024;
const MAX_CLI_OUTPUT_BYTES: usize = 384 * 1024;

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
    New {
        path: PathBuf,
        name: Option<String>,
    },
    Check {
        path: PathBuf,
    },
    Inspect {
        path: PathBuf,
        offset: usize,
        limit: usize,
    },
    Run {
        path: PathBuf,
        recover_save: bool,
    },
    Test {
        path: PathBuf,
        name: Option<String>,
    },
    Replay {
        project_path: PathBuf,
        replay_path: PathBuf,
    },
    Screenshot {
        project_path: PathBuf,
        scene_id: String,
        output_path: PathBuf,
    },
    Edit {
        project_path: PathBuf,
        relative_path: String,
        operation: SceneEditOperation,
        expected_original_sha256: Option<String>,
    },
    ResourceEdit {
        project_path: PathBuf,
        relative_path: String,
        operation: ResourceEditOperation,
        expected_original_sha256: Option<String>,
    },
    Build {
        project_path: PathBuf,
        output_path: PathBuf,
    },
    Package {
        bundle_path: PathBuf,
        archive_path: PathBuf,
    },
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::Version => "version",
            Self::New { .. } => "new",
            Self::Check { .. } => "check",
            Self::Inspect { .. } => "inspect",
            Self::Run { .. } => "run",
            Self::Test { .. } => "test",
            Self::Replay { .. } => "replay",
            Self::Screenshot { .. } => "screenshot",
            Self::Edit { .. } => "edit",
            Self::ResourceEdit { .. } => "resource-edit",
            Self::Build { .. } => "build",
            Self::Package { .. } => "package",
        }
    }
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
struct TestInfo {
    name: String,
    passed: bool,
    message: String,
}

#[derive(Serialize)]
struct TestsResult {
    tests: Vec<TestInfo>,
    passed_count: usize,
    failed_count: usize,
}

#[derive(Serialize)]
struct ReplayResult {
    frame_count: usize,
    scene_id: String,
    player_position_milli: [i64; 2],
    completed: bool,
    state_hash: String,
}

#[derive(Serialize)]
struct ScreenshotResult {
    scene_id: String,
    output_path: String,
    bytes_written: usize,
    format: &'static str,
}

#[derive(Serialize)]
struct EditCommandResult {
    applied: bool,
    relative_path: String,
    scene_id: String,
    original_sha256: String,
    candidate_sha256: String,
    diff: String,
    diff_truncated: bool,
    backup_path: Option<String>,
}

#[derive(Serialize)]
struct ResourceEditCommandResult {
    applied: bool,
    relative_path: String,
    resource_id: String,
    original_sha256: String,
    candidate_sha256: String,
    diff: String,
    diff_truncated: bool,
    backup_path: Option<String>,
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
    total_scene_count: usize,
    total_resource_count: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    dependencies: serde_json::Value,
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
/// - `hycel inspect [<project-path>] [--offset <n>] [--limit <1..1000>] [--json]`
/// - `hycel run <project-path> [--recover-save] [--json]`
/// - `hycel replay <project-path> <replay-file> [--json]`
/// - `hycel screenshot <project-path> --scene <scene-uuid> --output <file.svg> [--json]`
/// - `hycel edit <project-path> --file <scenes/file.json> --operation-json <json> [--apply <sha256>] [--json]`
/// - `hycel resource-edit <project-path> --file <assets/file.hycel.json> --operation-json <json> [--apply <sha256>] [--json]`
/// - `hycel build <project-path> --output <new-directory> [--json]`
/// - `hycel package <bundle-directory> --output <new-file.tar> [--json]`
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
        Ok(parsed) => {
            let command = parsed.command.name();
            let json = parsed.json;
            let output = execute_command(parsed.command, json);
            if output.stdout.len().saturating_add(output.stderr.len()) > MAX_CLI_OUTPUT_BYTES {
                failure(
                    command,
                    3,
                    vec![cli_diagnostic(
                        "HYCEL-CLI-199",
                        None,
                        "output",
                        format!(
                            "command output exceeds the {MAX_CLI_OUTPUT_BYTES}-byte limit; use paginated inspect or narrow the request"
                        ),
                    )],
                    json,
                )
            } else {
                output
            }
        }
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
        "inspect" => parse_inspect_arguments(rest)?,
        "run" => parse_run_arguments(rest)?,
        "test" => parse_test_arguments(rest)?,
        "replay" => parse_replay_arguments(rest)?,
        "screenshot" => parse_screenshot_arguments(rest)?,
        "edit" => parse_edit_arguments(rest)?,
        "resource-edit" => parse_resource_edit_arguments(rest)?,
        "build" => parse_output_directory_arguments("build", rest)?,
        "package" => parse_package_arguments(rest)?,
        _ => return Err((command.to_owned(), format!("unknown command: {command}"))),
    };
    Ok(ParsedArgs {
        command: parsed,
        json,
    })
}

fn parse_inspect_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut path = None;
    let mut offset = 0_usize;
    let mut limit = 100_usize;
    let mut saw_offset = false;
    let mut saw_limit = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].to_str() {
            Some("--offset" | "--limit") => {
                let option = arguments[index].to_str().unwrap_or_default();
                if index + 1 >= arguments.len() {
                    return Err((
                        "inspect".to_owned(),
                        format!("{option} requires an integer"),
                    ));
                }
                let value = arguments[index + 1]
                    .to_str()
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or_else(|| {
                        (
                            "inspect".to_owned(),
                            format!("{option} must be a non-negative integer"),
                        )
                    })?;
                match option {
                    "--offset" if !saw_offset => {
                        offset = value;
                        saw_offset = true;
                    }
                    "--limit" if !saw_limit => {
                        limit = value;
                        saw_limit = true;
                    }
                    _ => {
                        return Err((
                            "inspect".to_owned(),
                            format!("{option} may be used only once"),
                        ));
                    }
                }
                index += 2;
            }
            Some(option) if option.starts_with('-') => {
                return Err(("inspect".to_owned(), format!("unknown option: {option}")));
            }
            _ => {
                if path.replace(PathBuf::from(&arguments[index])).is_some() {
                    return Err((
                        "inspect".to_owned(),
                        "inspect accepts at most one project path".to_owned(),
                    ));
                }
                index += 1;
            }
        }
    }
    if !(1..=1_000).contains(&limit) {
        return Err((
            "inspect".to_owned(),
            "--limit must be from 1 through 1000".to_owned(),
        ));
    }
    if offset > 100_000 {
        return Err((
            "inspect".to_owned(),
            "--offset must be no greater than 100000".to_owned(),
        ));
    }
    Ok(Command::Inspect {
        path: path.unwrap_or_else(|| PathBuf::from(".")),
        offset,
        limit,
    })
}

fn parse_run_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut path = None;
    let mut recover_save = false;
    for argument in arguments {
        if argument == "--recover-save" {
            if recover_save {
                return Err((
                    "run".to_owned(),
                    "--recover-save may be used only once".to_owned(),
                ));
            }
            recover_save = true;
        } else if argument.to_string_lossy().starts_with('-') {
            return Err((
                "run".to_owned(),
                format!("unknown option: {}", argument.to_string_lossy()),
            ));
        } else if path.replace(PathBuf::from(argument)).is_some() {
            return Err((
                "run".to_owned(),
                "run accepts exactly one project path".to_owned(),
            ));
        }
    }
    let path = path.ok_or_else(|| ("run".to_owned(), "run requires a project path".to_owned()))?;
    Ok(Command::Run { path, recover_save })
}

fn parse_output_directory_arguments(
    command: &str,
    arguments: &[OsString],
) -> Result<Command, (String, String)> {
    let mut input_path = None;
    let mut output_path = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--output" {
            if output_path.is_some() || index + 1 >= arguments.len() {
                return Err((
                    command.to_owned(),
                    "--output requires one new directory".to_owned(),
                ));
            }
            output_path = Some(PathBuf::from(&arguments[index + 1]));
            index += 2;
        } else if arguments[index].to_string_lossy().starts_with('-') {
            return Err((
                command.to_owned(),
                format!("unknown option: {}", arguments[index].to_string_lossy()),
            ));
        } else {
            if input_path
                .replace(PathBuf::from(&arguments[index]))
                .is_some()
            {
                return Err((
                    command.to_owned(),
                    format!("{command} accepts one input path"),
                ));
            }
            index += 1;
        }
    }
    let input_path = input_path.ok_or_else(|| {
        (
            command.to_owned(),
            format!("{command} requires an input path"),
        )
    })?;
    let output_path = output_path.ok_or_else(|| {
        (
            command.to_owned(),
            "build requires --output <new-directory>".to_owned(),
        )
    })?;
    if command == "build" {
        Ok(Command::Build {
            project_path: input_path,
            output_path,
        })
    } else {
        Err((
            command.to_owned(),
            "unsupported output-directory command".to_owned(),
        ))
    }
}

fn parse_package_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    if arguments.len() != 3
        || arguments[1] != "--output"
        || arguments[0].to_string_lossy().starts_with('-')
    {
        return Err((
            "package".to_owned(),
            "package requires <bundle-directory> --output <new-file.tar>".to_owned(),
        ));
    }
    let archive_path = PathBuf::from(&arguments[2]);
    if !archive_path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tar"))
    {
        return Err((
            "package".to_owned(),
            "package output must use the .tar extension".to_owned(),
        ));
    }
    Ok(Command::Package {
        bundle_path: PathBuf::from(&arguments[0]),
        archive_path,
    })
}

fn parse_edit_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut project_path = None;
    let mut relative_path = None;
    let mut operation = None;
    let mut expected_original_sha256 = None;
    let mut index = 0;
    while index < arguments.len() {
        let value = arguments[index].to_str();
        if let Some(option) = value.filter(|value| value.starts_with('-')) {
            if index + 1 >= arguments.len() {
                return Err(("edit".to_owned(), format!("{option} requires a value")));
            }
            match option {
                "--file" if relative_path.is_none() => {
                    relative_path = Some(
                        arguments[index + 1]
                            .to_str()
                            .ok_or_else(|| {
                                ("edit".to_owned(), "scene path must be UTF-8".to_owned())
                            })?
                            .to_owned(),
                    );
                }
                "--operation-json" if operation.is_none() => {
                    let bytes = arguments[index + 1].to_str().ok_or_else(|| {
                        ("edit".to_owned(), "operation JSON must be UTF-8".to_owned())
                    })?;
                    operation = Some(serde_json::from_str::<SceneEditOperation>(bytes).map_err(
                        |error| {
                            (
                                "edit".to_owned(),
                                format!("invalid typed edit operation: {error}"),
                            )
                        },
                    )?);
                }
                "--apply" if expected_original_sha256.is_none() => {
                    expected_original_sha256 = Some(
                        arguments[index + 1]
                            .to_str()
                            .ok_or_else(|| {
                                (
                                    "edit".to_owned(),
                                    "expected SHA-256 must be UTF-8".to_owned(),
                                )
                            })?
                            .to_owned(),
                    );
                }
                _ => {
                    return Err((
                        "edit".to_owned(),
                        format!("unknown or duplicate option: {option}"),
                    ));
                }
            }
            index += 2;
        } else {
            if project_path
                .replace(PathBuf::from(&arguments[index]))
                .is_some()
            {
                return Err((
                    "edit".to_owned(),
                    "edit accepts one project path".to_owned(),
                ));
            }
            index += 1;
        }
    }
    Ok(Command::Edit {
        project_path: project_path
            .ok_or_else(|| ("edit".to_owned(), "edit requires a project path".to_owned()))?,
        relative_path: relative_path.ok_or_else(|| {
            (
                "edit".to_owned(),
                "edit requires --file <scene-path>".to_owned(),
            )
        })?,
        operation: operation.ok_or_else(|| {
            (
                "edit".to_owned(),
                "edit requires --operation-json <json>".to_owned(),
            )
        })?,
        expected_original_sha256,
    })
}

fn parse_resource_edit_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut project_path = None;
    let mut relative_path = None;
    let mut operation = None;
    let mut expected_original_sha256 = None;
    let mut index = 0;
    while index < arguments.len() {
        let value = arguments[index].to_str();
        if let Some(option) = value.filter(|value| value.starts_with('-')) {
            if index + 1 >= arguments.len() {
                return Err((
                    "resource-edit".to_owned(),
                    format!("{option} requires a value"),
                ));
            }
            match option {
                "--file" if relative_path.is_none() => {
                    relative_path = Some(
                        arguments[index + 1]
                            .to_str()
                            .ok_or_else(|| {
                                (
                                    "resource-edit".to_owned(),
                                    "resource path must be UTF-8".to_owned(),
                                )
                            })?
                            .to_owned(),
                    );
                }
                "--operation-json" if operation.is_none() => {
                    let bytes = arguments[index + 1].to_str().ok_or_else(|| {
                        (
                            "resource-edit".to_owned(),
                            "operation JSON must be UTF-8".to_owned(),
                        )
                    })?;
                    operation = Some(
                        serde_json::from_str::<ResourceEditOperation>(bytes).map_err(|error| {
                            (
                                "resource-edit".to_owned(),
                                format!("invalid typed resource operation: {error}"),
                            )
                        })?,
                    );
                }
                "--apply" if expected_original_sha256.is_none() => {
                    expected_original_sha256 = Some(
                        arguments[index + 1]
                            .to_str()
                            .ok_or_else(|| {
                                (
                                    "resource-edit".to_owned(),
                                    "expected SHA-256 must be UTF-8".to_owned(),
                                )
                            })?
                            .to_owned(),
                    );
                }
                _ => {
                    return Err((
                        "resource-edit".to_owned(),
                        format!("unknown or duplicate option: {option}"),
                    ));
                }
            }
            index += 2;
        } else {
            if project_path
                .replace(PathBuf::from(&arguments[index]))
                .is_some()
            {
                return Err((
                    "resource-edit".to_owned(),
                    "resource-edit accepts one project path".to_owned(),
                ));
            }
            index += 1;
        }
    }
    Ok(Command::ResourceEdit {
        project_path: project_path.ok_or_else(|| {
            (
                "resource-edit".to_owned(),
                "resource-edit requires a project path".to_owned(),
            )
        })?,
        relative_path: relative_path.ok_or_else(|| {
            (
                "resource-edit".to_owned(),
                "resource-edit requires --file <descriptor-path>".to_owned(),
            )
        })?,
        operation: operation.ok_or_else(|| {
            (
                "resource-edit".to_owned(),
                "resource-edit requires --operation-json <json>".to_owned(),
            )
        })?,
        expected_original_sha256,
    })
}

fn parse_screenshot_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut project_path = None;
    let mut scene_id = None;
    let mut output_path = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].to_str() {
            Some("--scene") => {
                if scene_id.is_some() || index + 1 >= arguments.len() {
                    return Err((
                        "screenshot".to_owned(),
                        "--scene requires one scene UUID".to_owned(),
                    ));
                }
                scene_id = Some(
                    arguments[index + 1]
                        .to_str()
                        .ok_or_else(|| {
                            (
                                "screenshot".to_owned(),
                                "scene UUID must be UTF-8".to_owned(),
                            )
                        })?
                        .to_owned(),
                );
                index += 2;
            }
            Some("--output") => {
                if output_path.is_some() || index + 1 >= arguments.len() {
                    return Err((
                        "screenshot".to_owned(),
                        "--output requires one file path".to_owned(),
                    ));
                }
                output_path = Some(PathBuf::from(&arguments[index + 1]));
                index += 2;
            }
            Some(value) if value.starts_with('-') => {
                return Err(("screenshot".to_owned(), format!("unknown option: {value}")));
            }
            _ => {
                if project_path
                    .replace(PathBuf::from(&arguments[index]))
                    .is_some()
                {
                    return Err((
                        "screenshot".to_owned(),
                        "screenshot accepts one project path".to_owned(),
                    ));
                }
                index += 1;
            }
        }
    }
    let output_path = output_path.ok_or_else(|| {
        (
            "screenshot".to_owned(),
            "screenshot requires --output <file.svg>".to_owned(),
        )
    })?;
    if !output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("svg"))
    {
        return Err((
            "screenshot".to_owned(),
            "screenshot output must use the .svg extension".to_owned(),
        ));
    }
    Ok(Command::Screenshot {
        project_path: project_path.ok_or_else(|| {
            (
                "screenshot".to_owned(),
                "screenshot requires a project path".to_owned(),
            )
        })?,
        scene_id: scene_id.ok_or_else(|| {
            (
                "screenshot".to_owned(),
                "screenshot requires --scene <scene-uuid>".to_owned(),
            )
        })?,
        output_path,
    })
}

fn parse_replay_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    if arguments.len() != 2 {
        return Err((
            "replay".to_owned(),
            "replay requires a project path and one replay JSON file".to_owned(),
        ));
    }
    if arguments
        .iter()
        .any(|argument| argument.to_string_lossy().starts_with('-'))
    {
        return Err((
            "replay".to_owned(),
            "replay paths must not be options".to_owned(),
        ));
    }
    Ok(Command::Replay {
        project_path: PathBuf::from(&arguments[0]),
        replay_path: PathBuf::from(&arguments[1]),
    })
}

fn parse_test_arguments(arguments: &[OsString]) -> Result<Command, (String, String)> {
    let mut path = None;
    let mut name = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--test" {
            if name.is_some() || index + 1 >= arguments.len() {
                return Err((
                    "test".to_owned(),
                    "--test requires one scenario name and may be used only once".to_owned(),
                ));
            }
            name = Some(
                arguments[index + 1]
                    .to_str()
                    .ok_or_else(|| ("test".to_owned(), "scenario name must be UTF-8".to_owned()))?
                    .to_owned(),
            );
            index += 2;
        } else if arguments[index].to_string_lossy().starts_with('-') {
            return Err((
                "test".to_owned(),
                format!("unknown option: {}", arguments[index].to_string_lossy()),
            ));
        } else if path.replace(PathBuf::from(&arguments[index])).is_some() {
            return Err((
                "test".to_owned(),
                "test accepts exactly one project path".to_owned(),
            ));
        } else {
            index += 1;
        }
    }
    let path =
        path.ok_or_else(|| ("test".to_owned(), "test requires a project path".to_owned()))?;
    Ok(Command::Test { path, name })
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

#[allow(clippy::too_many_lines)] // Keep command routing centralized and directly auditable.
fn execute_command(command: Command, json: bool) -> CliOutput {
    match command {
        Command::Help => {
            let result = HelpResult {
                usage: "hycel <new|check|inspect|run|test|replay|screenshot|edit|resource-edit|build|package> [arguments] [--json]",
                commands: vec![
                    "new <path> [--name <display-name>]",
                    "check [<project-path>]",
                    "inspect [<project-path>] [--offset <n>] [--limit <1..1000>]",
                    "run <project-path> [--recover-save]",
                    "test <project-path> [--test <scenario>]",
                    "replay <project-path> <replay-file>",
                    "screenshot <project-path> --scene <scene-uuid> --output <file.svg>",
                    "edit <project-path> --file <scenes/file.json> --operation-json <json> [--apply <sha256>]",
                    "resource-edit <project-path> --file <assets/file.hycel.json> --operation-json <json> [--apply <sha256>]",
                    "build <project-path> --output <new-directory>",
                    "package <bundle-directory> --output <new-file.tar>",
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
                "Hycel project tools\nRun `hycel new <path>`, `hycel check`, `hycel inspect`, `hycel test`, `hycel replay`, or `hycel run <project-path>`.\n",
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
        Command::Inspect {
            path,
            offset,
            limit,
        } => inspect_project(&path, offset, limit, json),
        Command::Run { path, recover_save } => {
            match hycel_demo::run_playable_platformer(&path, recover_save) {
                Ok(()) => success(
                    "run",
                    serde_json::json!({"project_path": path.to_string_lossy()}),
                    json,
                    "Game closed.\n",
                ),
                Err(error) => failure(
                    "run",
                    3,
                    vec![cli_diagnostic(
                        "HYCEL-CLI-120",
                        path.to_str(),
                        "runtime",
                        format!("built-in reference game failed: {error}"),
                    )],
                    json,
                ),
            }
        }
        Command::Test { path, name } => test_project(&path, name.as_deref(), json),
        Command::Replay {
            project_path,
            replay_path,
        } => replay_project(&project_path, &replay_path, json),
        Command::Screenshot {
            project_path,
            scene_id,
            output_path,
        } => screenshot_scene(&project_path, &scene_id, &output_path, json),
        Command::Edit {
            project_path,
            relative_path,
            operation,
            expected_original_sha256,
        } => edit_project(
            &project_path,
            &relative_path,
            &operation,
            expected_original_sha256.as_deref(),
            json,
        ),
        Command::ResourceEdit {
            project_path,
            relative_path,
            operation,
            expected_original_sha256,
        } => resource_edit_project(
            &project_path,
            &relative_path,
            &operation,
            expected_original_sha256.as_deref(),
            json,
        ),
        Command::Build {
            project_path,
            output_path,
        } => bundle::build_project(&project_path, &output_path, json),
        Command::Package {
            bundle_path,
            archive_path,
        } => bundle::package_bundle(&bundle_path, &archive_path, json),
    }
}

fn edit_project(
    project_path: &Path,
    relative_path: &str,
    operation: &SceneEditOperation,
    expected_original_sha256: Option<&str>,
    json: bool,
) -> CliOutput {
    let preview = match preview_scene_edit(project_path, relative_path, operation) {
        Ok(preview) => preview,
        Err(error) => {
            return failure(
                "edit",
                1,
                vec![cli_diagnostic(
                    "HYCEL-CLI-140",
                    Some(relative_path),
                    "edit",
                    error,
                )],
                json,
            );
        }
    };
    let receipt = if let Some(expected_sha256) = expected_original_sha256 {
        match apply_scene_edit(project_path, relative_path, operation, expected_sha256) {
            Ok(receipt) => Some(receipt),
            Err(error) => {
                return failure(
                    "edit",
                    1,
                    vec![cli_diagnostic(
                        "HYCEL-CLI-141",
                        Some(relative_path),
                        "apply",
                        error,
                    )],
                    json,
                );
            }
        }
    } else {
        None
    };
    let result = EditCommandResult {
        applied: receipt.is_some(),
        relative_path: preview.relative_path,
        scene_id: preview.scene_id,
        original_sha256: preview.original_sha256,
        candidate_sha256: preview.candidate_sha256,
        diff: preview.diff,
        diff_truncated: preview.diff_truncated,
        backup_path: receipt.and_then(|receipt| receipt.backup_path),
    };
    let status = match (result.applied, result.backup_path.as_deref()) {
        (true, Some(backup)) => format!("Applied edit; backup={backup}\n"),
        (true, None) => "Created scene; no prior file required a backup.\n".to_owned(),
        (false, _) => {
            "Preview only; pass --apply <original_sha256> after reviewing the diff.\n".to_owned()
        }
    };
    let human = format!("{}{status}", result.diff);
    success("edit", result, json, &human)
}

fn resource_edit_project(
    project_path: &Path,
    relative_path: &str,
    operation: &ResourceEditOperation,
    expected_original_sha256: Option<&str>,
    json: bool,
) -> CliOutput {
    let preview = match preview_resource_edit(project_path, relative_path, operation) {
        Ok(preview) => preview,
        Err(error) => {
            return failure(
                "resource-edit",
                1,
                vec![cli_diagnostic(
                    "HYCEL-CLI-150",
                    Some(relative_path),
                    "resource_edit",
                    error,
                )],
                json,
            );
        }
    };
    let receipt = if let Some(expected_sha256) = expected_original_sha256 {
        match apply_resource_edit(project_path, relative_path, operation, expected_sha256) {
            Ok(receipt) => Some(receipt),
            Err(error) => {
                return failure(
                    "resource-edit",
                    1,
                    vec![cli_diagnostic(
                        "HYCEL-CLI-151",
                        Some(relative_path),
                        "apply",
                        error,
                    )],
                    json,
                );
            }
        }
    } else {
        None
    };
    let result = ResourceEditCommandResult {
        applied: receipt.is_some(),
        relative_path: preview.relative_path,
        resource_id: preview.resource_id,
        original_sha256: preview.original_sha256,
        candidate_sha256: preview.candidate_sha256,
        diff: preview.diff,
        diff_truncated: preview.diff_truncated,
        backup_path: receipt.map(|receipt| receipt.backup_path),
    };
    let status = if result.applied {
        format!("Applied resource edit; backup={:?}\n", result.backup_path)
    } else {
        "Preview only; pass --apply <original_sha256> after reviewing the diff.\n".to_owned()
    };
    let human = format!("{}{status}", result.diff);
    success("resource-edit", result, json, &human)
}

fn screenshot_scene(
    project_path: &Path,
    scene_id: &str,
    output_path: &Path,
    json: bool,
) -> CliOutput {
    let project = match load_project(project_path) {
        Ok(project) => project,
        Err(diagnostics) => return failure("screenshot", 1, diagnostics, json),
    };
    let Some((_, scene)) = project
        .scenes
        .iter()
        .find(|(_, scene)| scene.id() == scene_id)
    else {
        return failure(
            "screenshot",
            1,
            vec![cli_diagnostic(
                "HYCEL-CLI-130",
                None,
                "scene_id",
                format!("scene UUID {scene_id:?} was not found in the project"),
            )],
            json,
        );
    };
    let svg = match scene_svg(scene) {
        Ok(svg) => svg,
        Err(error) => {
            return failure(
                "screenshot",
                1,
                vec![cli_diagnostic("HYCEL-CLI-131", None, "scene", error)],
                json,
            );
        }
    };
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
    {
        Ok(file) => file,
        Err(error) => {
            return failure(
                "screenshot",
                3,
                vec![cli_diagnostic(
                    "HYCEL-CLI-132",
                    output_path.to_str(),
                    "output",
                    format!("cannot create screenshot without overwriting existing data: {error}"),
                )],
                json,
            );
        }
    };
    if let Err(error) = file
        .write_all(svg.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(output_path);
        return failure(
            "screenshot",
            3,
            vec![cli_diagnostic(
                "HYCEL-CLI-132",
                output_path.to_str(),
                "output",
                format!("cannot finish screenshot output: {error}"),
            )],
            json,
        );
    }
    let result = ScreenshotResult {
        scene_id: scene_id.to_owned(),
        output_path: output_path.to_string_lossy().into_owned(),
        bytes_written: svg.len(),
        format: "image/svg+xml",
    };
    success(
        "screenshot",
        result,
        json,
        &format!("Wrote SVG scene preview to {}\n", output_path.display()),
    )
}

const MAX_SCREENSHOT_SVG_BYTES: usize = 16 * 1024 * 1024;

fn scene_svg(scene: &SceneDocument) -> Result<String, String> {
    let mut svg = String::with_capacity(64 * 1024);
    svg.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"960\" height=\"540\" viewBox=\"0 0 960 540\">\n");
    svg.push_str("<rect width=\"960\" height=\"540\" fill=\"#101827\"/>\n");
    svg.push_str(
        "<text x=\"20\" y=\"30\" fill=\"#f5f7fa\" font-family=\"sans-serif\" font-size=\"18\">",
    );
    push_xml_escaped(&mut svg, scene.name())?;
    svg.push_str("</text>\n");
    for entity in scene.entities() {
        let position = entity.transform().translation_milli();
        let half = entity.transform().scale_milli();
        let center_x = 480_000_i128 + i128::from(position[0]) * 3 / 5;
        let center_y = 270_000_i128 + (i128::from(position[1]) - 350_000) * 7 / 10;
        let half_width = i128::from(half[0]) * 3 / 5;
        let half_height = i128::from(half[1]) * 7 / 10;
        let (color, layer) = if entity.tags().iter().any(|tag| tag == "platform") {
            ("#3d5a80", 0)
        } else if entity.tags().iter().any(|tag| tag == "hazard") {
            ("#d6455d", 1)
        } else if entity.tags().iter().any(|tag| tag == "checkpoint") {
            ("#edae49", 2)
        } else if entity.tags().iter().any(|tag| tag == "exit") {
            ("#2a9d8f", 3)
        } else if entity.tags().iter().any(|tag| tag == "collectible") {
            ("#4cc9f0", 3)
        } else if entity.tags().iter().any(|tag| tag == "player") {
            ("#f5f7fa", 4)
        } else {
            ("#8995a7", 0)
        };
        let degrees_milli = i128::from(entity.transform().rotation_units()) * 360_000 / 65_536;
        let center_horizontal = svg_milli(center_x);
        let center_vertical = svg_milli(center_y);
        let degrees_text = svg_milli(degrees_milli);
        let _ = writeln!(
            svg,
            "<g data-entity=\"{}\" data-layer=\"{layer}\" transform=\"rotate({degrees_text} {center_horizontal} {center_vertical})\">",
            entity.id()
        );
        let _ = write!(svg, "<title>");
        push_xml_escaped(&mut svg, entity.name())?;
        let _ = writeln!(svg, "</title>");
        if entity.tags().iter().any(|tag| tag == "collectible") {
            let radius = svg_milli(half_width.min(half_height));
            let _ = writeln!(
                svg,
                "<circle cx=\"{center_horizontal}\" cy=\"{center_vertical}\" r=\"{radius}\" fill=\"{color}\" stroke=\"#ffffff\" stroke-width=\"2\"/>"
            );
        } else {
            let _ = writeln!(
                svg,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{color}\" stroke=\"#ffffff\" stroke-width=\"1\"/>",
                svg_milli(center_x - half_width),
                svg_milli(center_y - half_height),
                svg_milli(half_width * 2),
                svg_milli(half_height * 2)
            );
        }
        svg.push_str("</g>\n");
        if svg.len() > MAX_SCREENSHOT_SVG_BYTES {
            return Err(format!(
                "scene SVG exceeds the {MAX_SCREENSHOT_SVG_BYTES}-byte output limit"
            ));
        }
    }
    svg.push_str("</svg>\n");
    if svg.len() > MAX_SCREENSHOT_SVG_BYTES {
        return Err(format!(
            "scene SVG exceeds the {MAX_SCREENSHOT_SVG_BYTES}-byte output limit"
        ));
    }
    Ok(svg)
}

fn svg_milli(value: i128) -> String {
    let magnitude = value.unsigned_abs();
    format!(
        "{}{:.0}.{:03}",
        if value < 0 { "-" } else { "" },
        magnitude / 1_000,
        magnitude % 1_000
    )
}

fn push_xml_escaped(output: &mut String, value: &str) -> Result<(), String> {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
        if output.len() > MAX_SCREENSHOT_SVG_BYTES {
            return Err(format!(
                "scene SVG exceeds the {MAX_SCREENSHOT_SVG_BYTES}-byte output limit"
            ));
        }
    }
    Ok(())
}

fn replay_project(project_path: &Path, replay_path: &Path, json: bool) -> CliOutput {
    let mut bytes = Vec::with_capacity(64 * 1024);
    let read_result = File::open(replay_path).and_then(|file| {
        file.take(hycel_core::MAX_REPLAY_JSON_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
    });
    if let Err(error) = read_result {
        return failure(
            "replay",
            3,
            vec![cli_diagnostic(
                "HYCEL-CLI-123",
                replay_path.to_str(),
                "replay_file",
                format!("cannot read replay file: {error}"),
            )],
            json,
        );
    }
    if bytes.len() > hycel_core::MAX_REPLAY_JSON_BYTES {
        return failure(
            "replay",
            1,
            vec![cli_diagnostic(
                "HYCEL-CLI-124",
                replay_path.to_str(),
                "replay_file",
                format!(
                    "replay exceeds the {}-byte limit",
                    hycel_core::MAX_REPLAY_JSON_BYTES
                ),
            )],
            json,
        );
    }
    let outcome = match hycel_demo::replay_playable_platformer(project_path, &bytes) {
        Ok(outcome) => outcome,
        Err(error) => {
            return failure(
                "replay",
                1,
                vec![cli_diagnostic(
                    "HYCEL-CLI-125",
                    replay_path.to_str(),
                    "replay",
                    format!("reference game replay failed: {error}"),
                )],
                json,
            );
        }
    };
    let result = ReplayResult {
        frame_count: outcome.frame_count,
        scene_id: outcome.scene_id,
        player_position_milli: outcome.player_position_milli,
        completed: outcome.completed,
        state_hash: outcome.state_hash,
    };
    let human = format!(
        "Replayed {} tick(s); scene={}, player=({}, {}), completed={}, state_hash={}\n",
        result.frame_count,
        result.scene_id,
        result.player_position_milli[0],
        result.player_position_milli[1],
        result.completed,
        result.state_hash
    );
    success("replay", result, json, &human)
}

fn test_project(path: &Path, selected: Option<&str>, json: bool) -> CliOutput {
    let outcomes = match hycel_demo::test_playable_platformer(path, selected) {
        Ok(outcomes) => outcomes,
        Err(error) => {
            return failure(
                "test",
                1,
                vec![cli_diagnostic(
                    "HYCEL-CLI-121",
                    path.to_str(),
                    "tests",
                    format!("reference game tests could not run: {error}"),
                )],
                json,
            );
        }
    };
    let tests = outcomes
        .into_iter()
        .map(|outcome| TestInfo {
            name: outcome.name,
            passed: outcome.passed,
            message: outcome.message,
        })
        .collect::<Vec<_>>();
    let passed_count = tests.iter().filter(|test| test.passed).count();
    let failed_count = tests.len() - passed_count;
    let result = TestsResult {
        tests,
        passed_count,
        failed_count,
    };
    let mut human = String::new();
    for test in &result.tests {
        let _ = writeln!(
            human,
            "{} {}: {}",
            if test.passed { "PASS" } else { "FAIL" },
            test.name,
            test.message
        );
    }
    if failed_count == 0 {
        success("test", result, json, &human)
    } else {
        let diagnostics = result
            .tests
            .iter()
            .filter(|test| !test.passed)
            .map(|test| {
                cli_diagnostic(
                    "HYCEL-CLI-122",
                    path.to_str(),
                    &format!("tests.{}", test.name),
                    test.message.clone(),
                )
            })
            .collect();
        failure_with_result("test", 1, result, diagnostics, json, &human)
    }
}

struct LoadedProject {
    manifest: ProjectManifest,
    scenes: Vec<(String, SceneDocument)>,
    resources: Vec<(String, ResourceDescriptor)>,
    animation_clips: Vec<(String, AnimationClipDocument)>,
    dependencies: AssetDependencyReport,
}

/// Bounded, read-only preview of a validated scene edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SceneEditPreview {
    /// Project-relative scene path.
    pub relative_path: String,
    /// Stable scene UUID.
    pub scene_id: String,
    /// SHA-256 of the source bytes the preview was based on.
    pub original_sha256: String,
    /// SHA-256 of the canonical candidate bytes.
    pub candidate_sha256: String,
    /// Bounded unified diff; `diff_truncated` indicates omitted content.
    pub diff: String,
    /// Whether the diff exceeded its 64 KiB output limit.
    pub diff_truncated: bool,
}

/// Receipt for a successful scene edit or creation; existing source bytes are backed up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SceneEditReceipt {
    /// Project-relative scene path that was changed.
    pub relative_path: String,
    /// SHA-256 of the original source bytes.
    pub original_sha256: String,
    /// SHA-256 of the written candidate bytes.
    pub candidate_sha256: String,
    /// Project-relative backup path containing the exact original bytes, or `None` for creation.
    pub backup_path: Option<String>,
}

/// Bounded, read-only preview of a typed resource descriptor edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceEditPreview {
    /// Project-relative resource descriptor path.
    pub relative_path: String,
    /// Stable resource UUID.
    pub resource_id: String,
    /// SHA-256 of the source bytes the preview was based on.
    pub original_sha256: String,
    /// SHA-256 of the canonical candidate bytes.
    pub candidate_sha256: String,
    /// Bounded unified diff; `diff_truncated` indicates omitted content.
    pub diff: String,
    /// Whether the diff exceeded its 64 KiB output limit.
    pub diff_truncated: bool,
}

/// Receipt for a successful resource edit with a recoverable source backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceEditReceipt {
    /// Project-relative resource descriptor path that was changed.
    pub relative_path: String,
    /// SHA-256 of the original descriptor bytes.
    pub original_sha256: String,
    /// SHA-256 of the written candidate bytes.
    pub candidate_sha256: String,
    /// Project-relative backup path containing the exact original bytes.
    pub backup_path: String,
}

type ProjectDocuments = (
    Vec<(String, SceneDocument)>,
    Vec<(String, ResourceDescriptor)>,
    Vec<(String, AnimationClipDocument)>,
);

fn inspect_project(path: &Path, offset: usize, limit: usize, json: bool) -> CliOutput {
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
            let total_scene_count = project.scenes.len();
            let total_resource_count = project.resources.len();
            let scenes = project
                .scenes
                .iter()
                .skip(offset)
                .take(limit)
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
                .skip(offset)
                .take(limit)
                .map(|(file, resource)| ResourceInfo {
                    id: resource.id().to_owned(),
                    kind: resource.kind().to_owned(),
                    descriptor_file: file.clone(),
                    source: resource.source().to_owned(),
                })
                .collect::<Vec<_>>();
            let mut dependencies = match serde_json::to_value(&project.dependencies) {
                Ok(dependencies) => dependencies,
                Err(error) => {
                    return failure(
                        "inspect",
                        1,
                        vec![cli_diagnostic(
                            "HYCEL-CLI-102",
                            None,
                            "dependencies",
                            format!("cannot serialize dependency page: {error}"),
                        )],
                        json,
                    );
                }
            };
            page_dependency_map(&mut dependencies, "resources", offset, limit);
            page_dependency_map(&mut dependencies, "scenes", offset, limit);
            let next_offset = (offset.saturating_add(limit)
                < total_scene_count.max(total_resource_count))
            .then_some(offset.saturating_add(limit));
            let result = InspectResult {
                format_version: project.manifest.format_version(),
                project_id: project.manifest.project_id().to_owned(),
                project_name: project.manifest.project_name().to_owned(),
                default_profile: project.manifest.default_profile().to_owned(),
                profiles,
                scenes,
                resources,
                total_scene_count,
                total_resource_count,
                offset,
                limit,
                next_offset,
                dependencies,
            };
            let human = human_inspect(&result);
            success("inspect", result, json, &human)
        }
        Err(diagnostics) => failure("inspect", 1, diagnostics, json),
    }
}

fn page_dependency_map(
    dependencies: &mut serde_json::Value,
    key: &str,
    offset: usize,
    limit: usize,
) {
    let Some(map) = dependencies
        .get_mut(key)
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let page = map
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    *map = page;
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

    let mut component_registry = ComponentRegistry::default();
    component_registry
        .register("hycel.animation", 1, false, ["clip_id"])
        .and_then(|()| component_registry.mark_resource_reference("hycel.animation", "clip_id"))
        .map_err(|error| vec![OutputDiagnostic::from(error)])?;
    let (scenes, resources, animation_clips) = parse_project_documents(
        &root,
        scene_files,
        resource_files,
        &component_registry,
        diagnostics,
    )?;
    let dependencies =
        AssetDependencyReport::build(&scenes, &resources, &component_registry, &animation_clips)
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
        animation_clips,
        dependencies,
    })
}

/// Validates a typed scene operation and returns a bounded preview without writing files.
///
/// # Errors
///
/// Returns an error for an invalid project/path/operation or a scene that is
/// too large to safely process.
pub fn preview_scene_edit(
    project_root: &Path,
    relative_path: &str,
    operation: &SceneEditOperation,
) -> Result<SceneEditPreview, String> {
    let (_, preview, _, _) = prepare_scene_edit(project_root, relative_path, operation)?;
    Ok(preview)
}

/// Applies a previously previewed operation only when the original content hash
/// still matches, saving the exact original bytes to a non-overwriting backup.
///
/// # Errors
///
/// Returns an error for stale previews, invalid project/path/operation data, or
/// failed backup/atomic-write operations.
pub fn apply_scene_edit(
    project_root: &Path,
    relative_path: &str,
    operation: &SceneEditOperation,
    expected_original_sha256: &str,
) -> Result<SceneEditReceipt, String> {
    let (scene_path, preview, original, candidate) =
        prepare_scene_edit(project_root, relative_path, operation)?;
    if preview.original_sha256 != expected_original_sha256 {
        return Err(format!(
            "scene changed since preview (expected {expected_original_sha256}, current {})",
            preview.original_sha256
        ));
    }
    if matches!(operation, SceneEditOperation::CreateScene { .. }) {
        return commit_scene_creation(project_root, relative_path, &preview, &original, &candidate);
    }
    if original == candidate {
        return Err("scene edit does not change the serialized source".to_owned());
    }
    let project_root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    let backup_directory = ensure_backup_directory(&project_root)?;
    let path_digest = hycel_assets::ContentHash::from_bytes(relative_path.as_bytes()).to_hex();
    let backup_name = format!("{path_digest}-{}.bak", preview.original_sha256);
    let backup_path = backup_directory.join(backup_name);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup_path)
    {
        Ok(mut backup) => {
            if let Err(error) = backup.write_all(&original).and_then(|()| backup.sync_all()) {
                drop(backup);
                let cleanup = fs::remove_file(&backup_path);
                let detail = cleanup.err().map_or_else(String::new, |cleanup_error| {
                    format!("; incomplete backup cleanup failed: {cleanup_error}")
                });
                return Err(format!(
                    "cannot finish original scene backup: {error}{detail}"
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = read_existing_backup(&backup_path, "scene")?;
            if existing != original {
                return Err("existing scene backup path contains different data".to_owned());
            }
        }
        Err(error) => return Err(format!("cannot create original scene backup: {error}")),
    }
    let metadata = fs::metadata(&scene_path)
        .map_err(|error| format!("cannot inspect scene before replacement: {error}"))?;
    let mut staged = AtomicWriteFile::open(&scene_path)
        .map_err(|error| format!("cannot stage atomic scene replacement: {error}"))?;
    staged
        .write_all(&candidate)
        .map_err(|error| format!("cannot write staged scene: {error}"))?;
    staged
        .set_permissions(metadata.permissions())
        .map_err(|error| format!("cannot preserve scene permissions: {error}"))?;
    staged
        .sync_all()
        .map_err(|error| format!("cannot sync staged scene: {error}"))?;
    let current = read_limited_bytes(&scene_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    if current != original {
        staged.discard().map_err(|error| {
            format!("scene changed during edit and staged cleanup failed: {error}")
        })?;
        return Err(
            "scene changed while editing; refusing to overwrite concurrent changes".to_owned(),
        );
    }
    staged
        .commit()
        .map_err(|error| format!("cannot atomically replace scene: {error}"))?;
    Ok(SceneEditReceipt {
        relative_path: relative_path.to_owned(),
        original_sha256: preview.original_sha256,
        candidate_sha256: preview.candidate_sha256,
        backup_path: Some(
            backup_path
                .strip_prefix(&project_root)
                .unwrap_or(&backup_path)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/"),
        ),
    })
}

const MAX_RESOURCE_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Validates a typed resource operation and returns a bounded preview without writing files.
///
/// # Errors
///
/// Returns an error for an invalid project, descriptor path, source file, or operation.
pub fn preview_resource_edit(
    project_root: &Path,
    relative_path: &str,
    operation: &ResourceEditOperation,
) -> Result<ResourceEditPreview, String> {
    let (_, preview, _, _) = prepare_resource_edit(project_root, relative_path, operation)?;
    Ok(preview)
}

/// Applies a resource edit only when the descriptor hash still matches its preview.
/// The exact original descriptor is stored in a non-overwriting backup first.
///
/// # Errors
///
/// Returns an error for stale previews, invalid resource data, or failed backup/replacement.
pub fn apply_resource_edit(
    project_root: &Path,
    relative_path: &str,
    operation: &ResourceEditOperation,
    expected_original_sha256: &str,
) -> Result<ResourceEditReceipt, String> {
    let (resource_path, preview, original, candidate) =
        prepare_resource_edit(project_root, relative_path, operation)?;
    if preview.original_sha256 != expected_original_sha256 {
        return Err(format!(
            "resource changed since preview (expected {expected_original_sha256}, current {})",
            preview.original_sha256
        ));
    }
    if original == candidate {
        return Err("resource edit does not change the serialized descriptor".to_owned());
    }
    let project_root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    let backup_directory = ensure_backup_directory(&project_root)?;
    let path_digest = hycel_assets::ContentHash::from_bytes(relative_path.as_bytes()).to_hex();
    let backup_path =
        backup_directory.join(format!("{path_digest}-{}.bak", preview.original_sha256));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup_path)
    {
        Ok(mut backup) => {
            if let Err(error) = backup.write_all(&original).and_then(|()| backup.sync_all()) {
                drop(backup);
                let cleanup = fs::remove_file(&backup_path);
                let detail = cleanup.err().map_or_else(String::new, |cleanup_error| {
                    format!("; incomplete backup cleanup failed: {cleanup_error}")
                });
                return Err(format!(
                    "cannot finish resource descriptor backup: {error}{detail}"
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = read_existing_backup(&backup_path, "resource")?;
            if existing != original {
                return Err("existing resource backup path contains different data".to_owned());
            }
        }
        Err(error) => return Err(format!("cannot create resource backup: {error}")),
    }
    let metadata = fs::metadata(&resource_path)
        .map_err(|error| format!("cannot inspect resource before replacement: {error}"))?;
    let mut staged = AtomicWriteFile::open(&resource_path)
        .map_err(|error| format!("cannot stage atomic resource replacement: {error}"))?;
    staged
        .write_all(&candidate)
        .map_err(|error| format!("cannot write staged resource descriptor: {error}"))?;
    staged
        .set_permissions(metadata.permissions())
        .map_err(|error| format!("cannot preserve resource permissions: {error}"))?;
    staged
        .sync_all()
        .map_err(|error| format!("cannot sync staged resource descriptor: {error}"))?;
    let current = read_limited_bytes(&resource_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    if current != original {
        staged.discard().map_err(|error| {
            format!("resource changed during edit and staged cleanup failed: {error}")
        })?;
        return Err(
            "resource changed while editing; refusing to overwrite concurrent changes".to_owned(),
        );
    }
    staged
        .commit()
        .map_err(|error| format!("cannot atomically replace resource descriptor: {error}"))?;
    Ok(ResourceEditReceipt {
        relative_path: relative_path.to_owned(),
        original_sha256: preview.original_sha256,
        candidate_sha256: preview.candidate_sha256,
        backup_path: backup_path
            .strip_prefix(&project_root)
            .unwrap_or(&backup_path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    })
}

fn prepare_resource_edit(
    project_root: &Path,
    relative_path: &str,
    operation: &ResourceEditOperation,
) -> Result<(PathBuf, ResourceEditPreview, Vec<u8>, Vec<u8>), String> {
    if let ResourceEditOperation::RestoreResourceBackup { backup_sha256 } = operation {
        return prepare_resource_restore(project_root, relative_path, backup_sha256);
    }
    validate_resource_edit_path(relative_path)?;
    let project = load_project(project_root)
        .map_err(|diagnostics| format_output_diagnostics(&diagnostics))?;
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    let resource_path = resolve_existing_project_path(&root, relative_path)
        .map_err(|diagnostic| diagnostic.to_string())?;
    let (_, indexed_resource) = project
        .resources
        .iter()
        .find(|(file, _)| file == relative_path)
        .ok_or_else(|| format!("validated resource descriptor {relative_path:?} was not found"))?;
    let original = read_limited_bytes(&resource_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    let registry = edit_resource_registry()?;
    let parsed = ResourceDescriptor::parse_json_with_registry(&original, relative_path, &registry)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    if parsed.id() != indexed_resource.id()
        || parsed.kind() != indexed_resource.kind()
        || parsed.source() != indexed_resource.source()
    {
        return Err("resource descriptor changed while preparing the edit".to_owned());
    }
    let edited = parsed
        .apply_edit(operation, relative_path, &registry)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    validate_resource_candidate(&root, relative_path, &project, &edited)?;

    let candidate = serde_json::to_vec_pretty(&edited)
        .map_err(|error| format!("cannot serialize resource candidate: {error}"))?;
    if candidate.len() > hycel_project::MAX_DOCUMENT_BYTES {
        return Err(format!(
            "candidate exceeds the {}-byte document limit",
            hycel_project::MAX_DOCUMENT_BYTES
        ));
    }
    let original_sha256 = hycel_assets::ContentHash::from_bytes(&original).to_hex();
    let candidate_sha256 = hycel_assets::ContentHash::from_bytes(&candidate).to_hex();
    let (diff, diff_truncated) = bounded_unified_diff(relative_path, &original, &candidate);
    let preview = ResourceEditPreview {
        relative_path: relative_path.to_owned(),
        resource_id: edited.id().to_owned(),
        original_sha256,
        candidate_sha256,
        diff,
        diff_truncated,
    };
    Ok((resource_path, preview, original, candidate))
}

fn prepare_resource_restore(
    project_root: &Path,
    relative_path: &str,
    backup_sha256: &str,
) -> Result<(PathBuf, ResourceEditPreview, Vec<u8>, Vec<u8>), String> {
    validate_resource_edit_path(relative_path)?;
    if backup_sha256.len() != 64
        || !backup_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("backup_sha256 must be 64 lowercase hexadecimal characters".to_owned());
    }
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    hycel_project::validate_project_root(&root)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    let project =
        load_project(&root).map_err(|diagnostics| format_output_diagnostics(&diagnostics))?;
    let resource_path = resolve_existing_project_path(&root, relative_path)
        .map_err(|diagnostic| diagnostic.to_string())?;
    let current = project
        .resources
        .iter()
        .find(|(file, _)| file == relative_path)
        .map(|(_, resource)| resource)
        .ok_or_else(|| format!("validated resource descriptor {relative_path:?} was not found"))?;
    let original = read_limited_bytes(&resource_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    let backup = read_scene_backup(&root, relative_path, backup_sha256)?;
    let registry = edit_resource_registry()?;
    let restored = ResourceDescriptor::parse_json_with_registry(&backup, relative_path, &registry)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    if restored.id() != current.id()
        || restored.kind() != current.kind()
        || restored.import_settings() != current.import_settings()
    {
        return Err(
            "resource backup identity or importer settings do not match the current descriptor"
                .to_owned(),
        );
    }
    validate_resource_candidate(&root, relative_path, &project, &restored)?;
    let original_sha256 = hycel_assets::ContentHash::from_bytes(&original).to_hex();
    let candidate_sha256 = hycel_assets::ContentHash::from_bytes(&backup).to_hex();
    let (diff, diff_truncated) = bounded_unified_diff(relative_path, &original, &backup);
    let preview = ResourceEditPreview {
        relative_path: relative_path.to_owned(),
        resource_id: restored.id().to_owned(),
        original_sha256,
        candidate_sha256,
        diff,
        diff_truncated,
    };
    Ok((resource_path, preview, original, backup))
}

fn validate_resource_candidate(
    root: &Path,
    relative_path: &str,
    project: &LoadedProject,
    edited: &ResourceDescriptor,
) -> Result<(), String> {
    hycel_assets::hash_source_file(root, edited.source(), MAX_RESOURCE_SOURCE_BYTES)
        .map_err(|error| format!("edited resource source is unavailable or unsafe: {error}"))?;
    let mut resources = project.resources.clone();
    let (_, target_resource) = resources
        .iter_mut()
        .find(|(file, _)| file == relative_path)
        .ok_or_else(|| format!("validated resource descriptor {relative_path:?} was not found"))?;
    *target_resource = edited.clone();
    let mut animation_clips = project.animation_clips.clone();
    if edited.kind() == "animation" {
        let mut total_bytes = 0;
        let bytes = read_project_document(root, edited.source(), &mut total_bytes)
            .map_err(|diagnostic| diagnostic.message)?;
        let clip = AnimationClipDocument::parse_json(&bytes, edited.source())
            .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
        if let Some((_, existing_clip)) = animation_clips
            .iter_mut()
            .find(|(resource_id, _)| resource_id == edited.id())
        {
            *existing_clip = clip;
        } else {
            animation_clips.push((edited.id().to_owned(), clip));
        }
    }
    let component_registry = edit_component_registry()?;
    validate_project_documents(&project.scenes, &resources, &component_registry)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    AssetDependencyReport::build(
        &project.scenes,
        &resources,
        &component_registry,
        &animation_clips,
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_resource_edit_path(relative_path: &str) -> Result<(), String> {
    if relative_path.chars().count() > 4096 {
        return Err("resource descriptor path must not exceed 4096 characters".to_owned());
    }
    if !relative_path.starts_with("assets/")
        || !relative_path.to_ascii_lowercase().ends_with(".hycel.json")
        || relative_path.contains('\\')
    {
        return Err(
            "resource edits are limited to project-relative assets/*.hycel.json descriptors"
                .to_owned(),
        );
    }
    Ok(())
}

fn edit_resource_registry() -> Result<ResourceRegistry, String> {
    let mut registry = ResourceRegistry::default();
    registry
        .register("texture", std::iter::empty::<&str>())
        .and_then(|()| registry.register("animation", std::iter::empty::<&str>()))
        .map_err(|error| error.to_string())?;
    Ok(registry)
}

const MAX_EDIT_DIFF_BYTES: usize = 64 * 1024;

fn prepare_scene_creation(
    project_root: &Path,
    relative_path: &str,
    scene_id: &str,
    name: &str,
) -> Result<(PathBuf, SceneEditPreview, Vec<u8>, Vec<u8>), String> {
    validate_scene_edit_path(relative_path)?;
    if relative_path.chars().count() > 4096
        || scene_id.chars().count() > 64
        || name.chars().count() > 128
    {
        return Err("scene path, UUID, or name exceeds its bounded length".to_owned());
    }
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    hycel_project::validate_project_root(&root)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    let _project =
        load_project(&root).map_err(|diagnostics| format_output_diagnostics(&diagnostics))?;
    let scene_path = resolve_new_scene_destination(&root, relative_path)?;
    let candidate_value = serde_json::json!({
        "schema_version": 2,
        "id": scene_id,
        "name": name,
        "entities": []
    });
    let mut input = serde_json::to_vec(&candidate_value)
        .map_err(|error| format!("cannot serialize new scene: {error}"))?;
    input.push(b'\n');
    let registry = edit_component_registry()?;
    let scene = SceneDocument::parse_json_with_registry(&input, relative_path, &registry)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    let candidate = scene
        .to_json_bytes()
        .map_err(|error| format!("cannot serialize new scene: {error}"))?;
    validate_project_with_scene_override(&root, relative_path, &candidate)?;
    let original = Vec::new();
    let original_sha256 = hycel_assets::ContentHash::from_bytes(&original).to_hex();
    let candidate_sha256 = hycel_assets::ContentHash::from_bytes(&candidate).to_hex();
    let (diff, diff_truncated) = bounded_unified_diff(relative_path, &original, &candidate);
    let preview = SceneEditPreview {
        relative_path: relative_path.to_owned(),
        scene_id: scene.id().to_owned(),
        original_sha256,
        candidate_sha256,
        diff,
        diff_truncated,
    };
    Ok((scene_path, preview, original, candidate))
}

fn resolve_new_scene_destination(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    hycel_project::validate_relative_project_path(relative_path)
        .map_err(|diagnostic| diagnostic.to_string())?;
    let relative = Path::new(relative_path);
    let parent = relative
        .parent()
        .ok_or_else(|| "scene path must have a parent directory".to_owned())?;
    let parent_text = parent
        .to_str()
        .ok_or_else(|| "scene parent path must be UTF-8".to_owned())?;
    let canonical_parent = resolve_existing_project_path(root, parent_text)
        .map_err(|diagnostic| diagnostic.to_string())?;
    let file_name = relative
        .file_name()
        .ok_or_else(|| "scene path must have a file name".to_owned())?;
    let destination = canonical_parent.join(file_name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => Err("scene creation target already exists".to_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(format!("cannot inspect scene creation target: {error}")),
    }
}

fn commit_scene_creation(
    project_root: &Path,
    relative_path: &str,
    preview: &SceneEditPreview,
    original: &[u8],
    candidate: &[u8],
) -> Result<SceneEditReceipt, String> {
    commit_scene_creation_with_ops(
        project_root,
        relative_path,
        preview,
        original,
        candidate,
        |temporary, destination| {
            fs::hard_link(temporary, destination)
                .map_err(|error| format!("cannot publish new scene without overwriting: {error}"))
        },
        cleanup_staged_scene,
    )
}

fn commit_scene_creation_with_ops<P, C>(
    project_root: &Path,
    relative_path: &str,
    preview: &SceneEditPreview,
    original: &[u8],
    candidate: &[u8],
    publish: P,
    cleanup_staged: C,
) -> Result<SceneEditReceipt, String>
where
    P: FnOnce(&Path, &Path) -> Result<(), String>,
    C: FnOnce(&Path) -> Result<(), String>,
{
    if !original.is_empty()
        || preview.original_sha256 != hycel_assets::ContentHash::from_bytes(&[]).to_hex()
    {
        return Err("scene creation preview is invalid".to_owned());
    }
    if candidate.len() > hycel_project::MAX_DOCUMENT_BYTES {
        return Err("new scene exceeds the document byte limit".to_owned());
    }
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    validate_project_with_scene_override(&root, relative_path, candidate)?;
    let destination = resolve_new_scene_destination(&root, relative_path)?;
    let file_name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "scene file name must be UTF-8".to_owned())?;
    let temporary = destination.with_file_name(format!(
        ".{file_name}.{}.tmp",
        new_uuid_v4().map_err(|error| format!("cannot generate staging identifier: {error}"))?
    ));
    let mut owns_temporary = false;
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create staged scene: {error}"))?;
        owns_temporary = true;
        file.write_all(candidate)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("cannot finish staged scene: {error}"))?;
        publish(&temporary, &destination)?;
        Ok(())
    })();
    let cleanup = if owns_temporary {
        cleanup_staged(&temporary)
    } else {
        Ok(())
    };
    if let Err(error) = write_result {
        if let Err(cleanup_error) = cleanup {
            return Err(format!(
                "{error}; staged scene cleanup failed: {cleanup_error}"
            ));
        }
        return Err(error);
    }
    if let Err(cleanup_error) = cleanup {
        return Err(format!(
            "scene was published at {relative_path}, but its temporary staging link could not be removed: {cleanup_error}"
        ));
    }
    Ok(SceneEditReceipt {
        relative_path: relative_path.to_owned(),
        original_sha256: preview.original_sha256.clone(),
        candidate_sha256: preview.candidate_sha256.clone(),
        backup_path: None,
    })
}

fn cleanup_staged_scene(path: &Path) -> Result<(), String> {
    fs::remove_file(path).map_err(|error| format!("cannot remove staging link: {error}"))
}

fn prepare_scene_edit(
    project_root: &Path,
    relative_path: &str,
    operation: &SceneEditOperation,
) -> Result<(PathBuf, SceneEditPreview, Vec<u8>, Vec<u8>), String> {
    if let SceneEditOperation::CreateScene { scene_id, name } = operation {
        return prepare_scene_creation(project_root, relative_path, scene_id, name);
    }
    if let SceneEditOperation::RestoreSceneBackup { backup_sha256 } = operation {
        return prepare_scene_restore(project_root, relative_path, backup_sha256);
    }
    validate_scene_edit_path(relative_path)?;
    let project = load_project(project_root)
        .map_err(|diagnostics| format_output_diagnostics(&diagnostics))?;
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    let scene_path = resolve_existing_project_path(&root, relative_path)
        .map_err(|diagnostic| diagnostic.to_string())?;
    let (_, scene) = project
        .scenes
        .iter()
        .find(|(file, _)| file == relative_path)
        .ok_or_else(|| format!("validated scene {relative_path:?} was not found"))?;
    let original = read_limited_bytes(&scene_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    let parsed = SceneDocument::parse_json_with_registry(
        &original,
        relative_path,
        &edit_component_registry()?,
    )
    .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    if parsed.id() != scene.id() {
        return Err("scene changed while preparing the edit".to_owned());
    }
    let edited = parsed
        .apply_edit(operation, relative_path, &edit_component_registry()?)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    let candidate = edited
        .to_json_bytes()
        .map_err(|error| format!("cannot serialize scene candidate: {error}"))?;
    if candidate.len() > hycel_project::MAX_DOCUMENT_BYTES {
        return Err(format!(
            "candidate exceeds the {}-byte document limit",
            hycel_project::MAX_DOCUMENT_BYTES
        ));
    }
    let original_sha256 = hycel_assets::ContentHash::from_bytes(&original).to_hex();
    let candidate_sha256 = hycel_assets::ContentHash::from_bytes(&candidate).to_hex();
    let (diff, diff_truncated) = bounded_unified_diff(relative_path, &original, &candidate);
    let preview = SceneEditPreview {
        relative_path: relative_path.to_owned(),
        scene_id: scene.id().to_owned(),
        original_sha256,
        candidate_sha256,
        diff,
        diff_truncated,
    };
    Ok((scene_path, preview, original, candidate))
}

fn prepare_scene_restore(
    project_root: &Path,
    relative_path: &str,
    backup_sha256: &str,
) -> Result<(PathBuf, SceneEditPreview, Vec<u8>, Vec<u8>), String> {
    validate_scene_edit_path(relative_path)?;
    if backup_sha256.len() != 64
        || !backup_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("backup_sha256 must be 64 lowercase hexadecimal characters".to_owned());
    }
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve project root: {error}"))?;
    hycel_project::validate_project_root(&root)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    let scene_path = resolve_existing_project_path(&root, relative_path)
        .map_err(|diagnostic| diagnostic.to_string())?;
    if !scene_path.is_file() {
        return Err("scene restore target must be an existing regular file".to_owned());
    }
    let original = read_limited_bytes(&scene_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    let backup = read_scene_backup(&root, relative_path, backup_sha256)?;
    validate_project_with_scene_override(&root, relative_path, &backup)?;
    let registry = edit_component_registry()?;
    let restored = SceneDocument::parse_json_with_registry(&backup, relative_path, &registry)
        .map_err(|diagnostics| format_project_diagnostics(&diagnostics))?;
    let original_sha256 = hycel_assets::ContentHash::from_bytes(&original).to_hex();
    let candidate_sha256 = hycel_assets::ContentHash::from_bytes(&backup).to_hex();
    let (diff, diff_truncated) = bounded_unified_diff(relative_path, &original, &backup);
    let preview = SceneEditPreview {
        relative_path: relative_path.to_owned(),
        scene_id: restored.id().to_owned(),
        original_sha256,
        candidate_sha256,
        diff,
        diff_truncated,
    };
    Ok((scene_path, preview, original, backup))
}

fn validate_scene_edit_path(relative_path: &str) -> Result<(), String> {
    if !relative_path.starts_with("scenes/")
        || !Path::new(relative_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        || relative_path.contains('\\')
    {
        return Err("scene edits are limited to project-relative scenes/*.json files".to_owned());
    }
    Ok(())
}

fn read_scene_backup(
    project_root: &Path,
    relative_path: &str,
    backup_sha256: &str,
) -> Result<Vec<u8>, String> {
    let private = project_root.join(".hycel");
    let backups = private.join("backups");
    for directory in [&private, &backups] {
        let metadata = fs::symlink_metadata(directory).map_err(|error| {
            format!(
                "cannot inspect backup directory {}: {error}",
                directory.display()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "backup directory {} must be a real directory, not a symlink",
                directory.display()
            ));
        }
    }
    let path_digest = hycel_assets::ContentHash::from_bytes(relative_path.as_bytes()).to_hex();
    let backup_path = backups.join(format!("{path_digest}-{backup_sha256}.bak"));
    let metadata = fs::symlink_metadata(&backup_path).map_err(|error| {
        format!(
            "cannot inspect scene backup {}: {error}",
            backup_path.display()
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("scene backup must be a regular, non-symlink file".to_owned());
    }
    let bytes = read_limited_bytes(&backup_path, hycel_project::MAX_DOCUMENT_BYTES)?;
    let actual = hycel_assets::ContentHash::from_bytes(&bytes).to_hex();
    if actual != backup_sha256 {
        return Err("scene backup content does not match backup_sha256".to_owned());
    }
    Ok(bytes)
}

fn validate_project_with_scene_override(
    project_root: &Path,
    relative_path: &str,
    replacement: &[u8],
) -> Result<(), String> {
    let mut directory_scan = DirectoryScan::default();
    let mut scene_files = collect_files(
        project_root,
        "scenes",
        |name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        },
        &mut directory_scan,
    );
    if !scene_files.iter().any(|file| file == relative_path) {
        scene_files.push(relative_path.to_owned());
        scene_files.sort();
    }
    let resource_files = collect_files(
        project_root,
        "assets",
        |name| name.to_ascii_lowercase().ends_with(".hycel.json"),
        &mut directory_scan,
    );
    if !directory_scan.diagnostics.is_empty() {
        return Err(format_output_diagnostics(&directory_scan.diagnostics));
    }
    let component_registry = edit_component_registry()?;
    let (scenes, resources, animation_clips) = parse_project_documents_with_scene_override(
        project_root,
        scene_files,
        resource_files,
        &component_registry,
        Vec::new(),
        Some((relative_path, replacement)),
    )
    .map_err(|diagnostics| format_output_diagnostics(&diagnostics))?;
    AssetDependencyReport::build(&scenes, &resources, &component_registry, &animation_clips)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn edit_component_registry() -> Result<ComponentRegistry, String> {
    let mut registry = ComponentRegistry::default();
    registry
        .register("hycel.animation", 1, false, ["clip_id"])
        .and_then(|()| registry.mark_resource_reference("hycel.animation", "clip_id"))
        .map_err(|error| error.to_string())?;
    Ok(registry)
}

fn read_limited_bytes(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(maximum.min(64 * 1024));
    File::open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > maximum {
        return Err(format!(
            "{} exceeds the {maximum}-byte read limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn read_existing_backup(path: &Path, kind: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect existing {kind} backup: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "existing {kind} backup must be a regular, non-symlink file"
        ));
    }
    read_limited_bytes(path, hycel_project::MAX_DOCUMENT_BYTES)
}

fn ensure_backup_directory(project_root: &Path) -> Result<PathBuf, String> {
    let private = project_root.join(".hycel");
    ensure_directory_without_symlink(&private)?;
    let backups = private.join("backups");
    ensure_directory_without_symlink(&backups)?;
    Ok(backups)
}

fn ensure_directory_without_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "{} must be a real directory, not a symlink or file",
            path.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path)
            .map_err(|error| format!("cannot create {}: {error}", path.display())),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

fn format_output_diagnostics(diagnostics: &[OutputDiagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_project_diagnostics(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn bounded_unified_diff(path: &str, original: &[u8], candidate: &[u8]) -> (String, bool) {
    let original = String::from_utf8_lossy(original);
    let candidate = String::from_utf8_lossy(candidate);
    let old_lines = original.lines().collect::<Vec<_>>();
    let new_lines = candidate.lines().collect::<Vec<_>>();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let context_start = prefix.saturating_sub(3);
    let old_middle_end = old_lines.len().saturating_sub(suffix);
    let new_middle_end = new_lines.len().saturating_sub(suffix);
    let context_end = (old_lines.len().min(new_lines.len()).saturating_sub(suffix) + 3)
        .min(old_lines.len())
        .min(new_lines.len());
    let mut output = String::new();
    let mut truncated = false;
    let header = format!("--- a/{path}\n+++ b/{path}\n");
    push_bounded_diff(&mut output, &header, &mut truncated);
    let old_count =
        old_middle_end.saturating_sub(context_start) + context_end.saturating_sub(old_middle_end);
    let new_count =
        new_middle_end.saturating_sub(context_start) + context_end.saturating_sub(new_middle_end);
    let hunk = format!(
        "@@ -{},{} +{},{} @@\n",
        context_start + 1,
        old_count,
        context_start + 1,
        new_count
    );
    push_bounded_diff(&mut output, &hunk, &mut truncated);
    for line in old_lines.iter().take(prefix).skip(context_start) {
        push_bounded_diff(&mut output, &format!(" {line}\n"), &mut truncated);
    }
    for line in old_lines
        .iter()
        .skip(prefix)
        .take(old_middle_end.saturating_sub(prefix))
    {
        push_bounded_diff(&mut output, &format!("-{line}\n"), &mut truncated);
    }
    for line in new_lines
        .iter()
        .skip(prefix)
        .take(new_middle_end.saturating_sub(prefix))
    {
        push_bounded_diff(&mut output, &format!("+{line}\n"), &mut truncated);
    }
    for line in old_lines
        .iter()
        .skip(old_middle_end)
        .take(context_end.saturating_sub(old_middle_end))
    {
        push_bounded_diff(&mut output, &format!(" {line}\n"), &mut truncated);
    }
    (output, truncated)
}

fn push_bounded_diff(output: &mut String, line: &str, truncated: &mut bool) {
    if output.len().saturating_add(line.len()) > MAX_EDIT_DIFF_BYTES {
        let remaining = MAX_EDIT_DIFF_BYTES.saturating_sub(output.len());
        let mut boundary = remaining.min(line.len());
        while !line.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        output.push_str(&line[..boundary]);
        *truncated = true;
    } else {
        output.push_str(line);
    }
}

fn parse_project_documents(
    root: &Path,
    scene_files: Vec<String>,
    resource_files: Vec<String>,
    component_registry: &ComponentRegistry,
    diagnostics: Vec<OutputDiagnostic>,
) -> Result<ProjectDocuments, Vec<OutputDiagnostic>> {
    parse_project_documents_with_scene_override(
        root,
        scene_files,
        resource_files,
        component_registry,
        diagnostics,
        None,
    )
}

fn parse_project_documents_with_scene_override(
    root: &Path,
    scene_files: Vec<String>,
    resource_files: Vec<String>,
    component_registry: &ComponentRegistry,
    mut diagnostics: Vec<OutputDiagnostic>,
    scene_override: Option<(&str, &[u8])>,
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
    let mut resource_registry = ResourceRegistry::default();
    resource_registry
        .register("texture", std::iter::empty::<&str>())
        .and_then(|()| resource_registry.register("animation", std::iter::empty::<&str>()))
        .map_err(|error| vec![OutputDiagnostic::from(error)])?;
    let mut total_bytes = 0_usize;
    let mut scenes = Vec::new();
    for file in scene_files {
        let document = read_scene_document(root, &file, &mut total_bytes, scene_override);
        match document {
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
    let mut animation_clips = Vec::new();
    for (_, resource) in &resources {
        if resource.kind() != "animation" {
            continue;
        }
        match read_project_document(root, resource.source(), &mut total_bytes) {
            Ok(bytes) => match AnimationClipDocument::parse_json(&bytes, resource.source()) {
                Ok(clip) => animation_clips.push((resource.id().to_owned(), clip)),
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
        Ok((scenes, resources, animation_clips))
    } else {
        Err(diagnostics)
    }
}

fn read_scene_document(
    root: &Path,
    file: &str,
    total_bytes: &mut usize,
    scene_override: Option<(&str, &[u8])>,
) -> Result<Vec<u8>, OutputDiagnostic> {
    if let Some((override_path, override_bytes)) = scene_override {
        if file == override_path {
            let remaining = MAX_PROJECT_DOCUMENT_BYTES.saturating_sub(*total_bytes);
            if override_bytes.len() > MAX_DOCUMENT_BYTES || override_bytes.len() > remaining {
                return Err(cli_diagnostic(
                    if override_bytes.len() > MAX_DOCUMENT_BYTES {
                        "HYCEL-DOCUMENT-001"
                    } else {
                        "HYCEL-CLI-112"
                    },
                    Some(file),
                    "$",
                    "scene replacement exceeds the bounded project document budget",
                ));
            }
            *total_bytes = total_bytes.saturating_add(override_bytes.len());
            return Ok(override_bytes.to_vec());
        }
    }
    read_project_document(root, file, total_bytes)
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
            "jump",
            vec![InputControl::Key {
                code: KeyCode::Space,
            }],
        )],
        vec![AxisBinding::new(
            0,
            "move_horizontal",
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
        result.project_name, result.project_id, result.default_profile, result.total_scene_count
    );
    let _ = writeln!(
        output,
        "Page offset={} limit={} ({} scene(s), {} resource(s)); next_offset={:?}",
        result.offset,
        result.limit,
        result.scenes.len(),
        result.resources.len(),
        result.next_offset
    );
    for scene in &result.scenes {
        let _ = writeln!(
            output,
            "  {} — {:?}: {} entities, {} components",
            scene.file, scene.name, scene.entity_count, scene.component_count
        );
    }
    let _ = writeln!(
        output,
        "Resources: {} / {}",
        result.resources.len(),
        result.total_resource_count
    );
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

fn failure_with_result<T: Serialize>(
    command: &str,
    exit_code: i32,
    result: T,
    diagnostics: Vec<OutputDiagnostic>,
    json: bool,
    human: &str,
) -> CliOutput {
    if json {
        CliOutput {
            exit_code,
            stdout: serialize_envelope(command, false, Some(result), diagnostics),
            stderr: String::new(),
        }
    } else {
        let mut output = failure(command, exit_code, diagnostics, false);
        human.clone_into(&mut output.stdout);
        output
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

    use hycel_core::{InputFrame as ReplayInputFrame, Replay};
    use hycel_input::InputBindings;
    use serde_json::{Value, json};

    use super::{
        apply_scene_edit, commit_scene_creation_with_ops, execute, prepare_scene_creation,
        preview_scene_edit,
    };
    use hycel_project::{SceneDocument, SceneEditOperation};

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
        assert_eq!(input.schema_version(), 2);
        assert_eq!(input.buttons().len(), 1);
        assert_eq!(input.axes().len(), 1);
        assert_eq!(input.action_id("jump"), Some(1));
        assert_eq!(input.action_id("move_horizontal"), Some(0));

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
    fn test_command_runs_named_headless_scenarios_with_versioned_results() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let output = execute(args(["test", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 0, "{}", output.stdout);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["command"], "test");
        assert_eq!(envelope["result"]["passed_count"], 5);
        assert_eq!(envelope["result"]["failed_count"], 0);

        let selected = execute(args([
            "test",
            project.to_str().unwrap(),
            "--test",
            "hazard-respawn",
            "--json",
        ]));
        assert_eq!(selected.exit_code, 0, "{}", selected.stdout);
        let envelope: Value = serde_json::from_str(&selected.stdout).unwrap();
        assert_eq!(envelope["result"]["tests"].as_array().unwrap().len(), 1);
        assert_eq!(envelope["result"]["tests"][0]["name"], "hazard-respawn");
    }

    #[test]
    fn replay_command_runs_bounded_tick_inputs_and_returns_a_state_hash() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let directory = test_directory("cli-replay");
        let replay_path = directory.join("walk.json");
        let mut frames = Vec::new();
        for tick in 0..8 {
            let mut frame = ReplayInputFrame::new(tick);
            frame.set_axis(0, i16::MAX);
            frame.set_button(1, tick == 0);
            frames.push(frame);
        }
        let replay = Replay::new(60, 0, 0, frames).unwrap();
        fs::write(&replay_path, replay.to_json().unwrap()).unwrap();

        let replay_args = [
            "replay",
            project.to_str().unwrap(),
            replay_path.to_str().unwrap(),
            "--json",
        ];
        let first = execute(args(replay_args));
        assert_eq!(first.exit_code, 0, "{}", first.stdout);
        let envelope: Value = serde_json::from_str(&first.stdout).unwrap();
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["result"]["frame_count"], 8);
        assert_eq!(envelope["result"]["state_hash"].as_str().unwrap().len(), 64);
        let second = execute(args(replay_args));
        assert_eq!(first.stdout, second.stdout);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bellglass_echo_replay_reproduces_canonical_gameplay_state() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/bellglass-courier");
        let directory = test_directory("bellglass-replay");
        let replay_path = directory.join("echo-flight.json");
        let frames = (0..100)
            .map(|tick| {
                let phase = tick % 120;
                let mut frame = ReplayInputFrame::new(tick);
                frame.set_axis(
                    0,
                    if phase < 28 || (55..63).contains(&phase) {
                        i16::MAX
                    } else {
                        0
                    },
                );
                frame.set_button(1, phase == 10);
                frame.set_button(3, phase == 55);
                frame
            })
            .collect();
        fs::write(
            &replay_path,
            Replay::new(60, 0, 0, frames).unwrap().to_json().unwrap(),
        )
        .unwrap();
        let replay_args = [
            "replay",
            project.to_str().unwrap(),
            replay_path.to_str().unwrap(),
            "--json",
        ];
        let first = execute(args(replay_args));
        assert_eq!(first.exit_code, 0, "{}", first.stdout);
        let first_json: Value = serde_json::from_str(&first.stdout).unwrap();
        assert_eq!(first_json["result"]["frame_count"], 100);
        let second = execute(args(replay_args));
        assert_eq!(first.stdout, second.stdout);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn entity_tag_edit_is_previewed_backed_up_and_revalidated() {
        let directory = test_directory("cli-entity-tags");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        let scene_path = project.join("scenes/first-room.json");
        let mut document: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
        document["entities"] = json!([{
            "id": "21000000-0000-4000-8000-000000000001",
            "name": "Courier",
            "tags": ["player"],
            "transform": {"translation_milli": [0, 0]},
            "components": []
        }]);
        fs::write(&scene_path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
        let original = fs::read(&scene_path).unwrap();
        let operation = SceneEditOperation::SetEntityTags {
            entity_id: "21000000-0000-4000-8000-000000000001".to_owned(),
            tags: vec!["player".to_owned(), "courier".to_owned()],
        };
        let preview = preview_scene_edit(&project, "scenes/first-room.json", &operation).unwrap();
        assert!(preview.diff.contains("courier"));
        assert_eq!(fs::read(&scene_path).unwrap(), original);
        let receipt = apply_scene_edit(
            &project,
            "scenes/first-room.json",
            &operation,
            &preview.original_sha256,
        )
        .unwrap();
        assert_eq!(
            fs::read(project.join(receipt.backup_path.unwrap())).unwrap(),
            original
        );
        let edited =
            SceneDocument::parse_json(&fs::read(&scene_path).unwrap(), "scenes/first-room.json")
                .unwrap();
        assert_eq!(edited.entities()[0].tags(), ["player", "courier"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scene_edit_is_previewed_hashed_backed_up_and_applied_only_against_current_bytes() {
        let directory = test_directory("cli-scene-edit");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        let scene_path = project.join("scenes/first-room.json");
        let original = fs::read(&scene_path).unwrap();
        let operation = SceneEditOperation::RenameScene {
            name: "The Quiet Observatory".to_owned(),
        };

        let preview = preview_scene_edit(&project, "scenes/first-room.json", &operation).unwrap();
        assert!(preview.diff.contains("The Quiet Observatory"));
        assert!(preview.diff.contains("First Room"));
        assert_eq!(preview.original_sha256.len(), 64);
        assert_eq!(preview.candidate_sha256.len(), 64);
        assert_eq!(fs::read(&scene_path).unwrap(), original);

        let stale =
            apply_scene_edit(&project, "scenes/first-room.json", &operation, "00").unwrap_err();
        assert!(stale.contains("changed since preview"));
        assert_eq!(fs::read(&scene_path).unwrap(), original);

        let receipt = apply_scene_edit(
            &project,
            "scenes/first-room.json",
            &operation,
            &preview.original_sha256,
        )
        .unwrap();
        assert_eq!(receipt.original_sha256, preview.original_sha256);
        assert_eq!(
            fs::read(project.join(receipt.backup_path.unwrap())).unwrap(),
            original
        );
        let edited_bytes = fs::read(&scene_path).unwrap();
        let edited = SceneDocument::parse_json(&edited_bytes, "room.json").unwrap();
        assert_eq!(edited.name(), "The Quiet Observatory");

        let corrupt_bytes = br#"{"interrupted":true}"#;
        fs::write(&scene_path, corrupt_bytes).unwrap();
        let corrupt_sha256 = hycel_assets::ContentHash::from_bytes(corrupt_bytes).to_hex();
        let restore = SceneEditOperation::RestoreSceneBackup {
            backup_sha256: receipt.original_sha256.clone(),
        };
        let restore_preview =
            preview_scene_edit(&project, "scenes/first-room.json", &restore).unwrap();
        assert_eq!(restore_preview.original_sha256, corrupt_sha256);
        assert_eq!(restore_preview.candidate_sha256, receipt.original_sha256);
        assert!(restore_preview.diff.contains("First Room"));
        let restore_receipt = apply_scene_edit(
            &project,
            "scenes/first-room.json",
            &restore,
            &restore_preview.original_sha256,
        )
        .unwrap();
        assert_eq!(fs::read(&scene_path).unwrap(), original);
        assert_eq!(
            fs::read(project.join(restore_receipt.backup_path.unwrap())).unwrap(),
            corrupt_bytes
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scene_creation_is_previewed_validated_and_never_overwrites_a_target() {
        let directory = test_directory("cli-scene-create");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap(), "--json"]));
        assert_eq!(created.exit_code, 0, "{}", created.stdout);
        let target = project.join("scenes/second-room.json");
        let original_scene = fs::read(project.join("scenes/first-room.json")).unwrap();
        let operation = r#"{"operation":"create_scene","scene_id":"11000000-0000-4000-8000-000000000009","name":"Second Room"}"#;
        let preview = execute(args([
            "edit",
            project.to_str().unwrap(),
            "--file",
            "scenes/second-room.json",
            "--operation-json",
            operation,
            "--json",
        ]));
        assert_eq!(preview.exit_code, 0, "{}", preview.stdout);
        assert!(!target.exists());
        assert_eq!(
            fs::read(project.join("scenes/first-room.json")).unwrap(),
            original_scene
        );
        let preview: Value = serde_json::from_str(&preview.stdout).unwrap();
        let empty_hash = hycel_assets::ContentHash::from_bytes(&[]).to_hex();
        assert_eq!(preview["result"]["original_sha256"], empty_hash);
        assert_eq!(preview["result"]["backup_path"], Value::Null);

        let applied = execute(args([
            "edit",
            project.to_str().unwrap(),
            "--file",
            "scenes/second-room.json",
            "--operation-json",
            operation,
            "--apply",
            &empty_hash,
            "--json",
        ]));
        assert_eq!(applied.exit_code, 0, "{}", applied.stdout);
        let applied: Value = serde_json::from_str(&applied.stdout).unwrap();
        assert_eq!(applied["result"]["backup_path"], Value::Null);
        let scene = fs::read_to_string(&target).unwrap();
        assert!(scene.contains("Second Room"));
        assert_eq!(
            execute(args(["check", project.to_str().unwrap(), "--json"])).exit_code,
            0
        );

        let changed = b"do not overwrite this file";
        fs::write(&target, changed).unwrap();
        let rejected = execute(args([
            "edit",
            project.to_str().unwrap(),
            "--file",
            "scenes/second-room.json",
            "--operation-json",
            operation,
            "--apply",
            &empty_hash,
            "--json",
        ]));
        assert_eq!(rejected.exit_code, 1);
        assert_eq!(fs::read(&target).unwrap(), changed);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scene_creation_reports_staging_cleanup_failures_before_and_after_publish() {
        let directory = test_directory("cli-scene-staging-cleanup");
        let project = directory.join("project");
        assert_eq!(
            execute(args(["new", project.to_str().unwrap()])).exit_code,
            0
        );
        let relative = "scenes/second-room.json";
        let target = project.join(relative);
        let (_, preview, original, candidate) = prepare_scene_creation(
            &project,
            relative,
            "11000000-0000-4000-8000-000000000009",
            "Second Room",
        )
        .unwrap();
        let failed_publish = commit_scene_creation_with_ops(
            &project,
            relative,
            &preview,
            &original,
            &candidate,
            |_, _| Err("injected publish failure".to_owned()),
            |temporary| {
                fs::remove_file(temporary).unwrap();
                Err("injected cleanup failure".to_owned())
            },
        )
        .unwrap_err();
        assert!(failed_publish.contains("injected publish failure"));
        assert!(failed_publish.contains("injected cleanup failure"));
        assert!(!target.exists());

        let (_, preview, original, candidate) = prepare_scene_creation(
            &project,
            relative,
            "11000000-0000-4000-8000-000000000009",
            "Second Room",
        )
        .unwrap();
        let published_cleanup_failure = commit_scene_creation_with_ops(
            &project,
            relative,
            &preview,
            &original,
            &candidate,
            |temporary, destination| {
                fs::hard_link(temporary, destination).map_err(|error| error.to_string())
            },
            |temporary| {
                fs::remove_file(temporary).unwrap();
                Err("injected cleanup failure".to_owned())
            },
        )
        .unwrap_err();
        assert!(published_cleanup_failure.contains("scene was published"));
        assert!(published_cleanup_failure.contains("injected cleanup failure"));
        assert!(target.is_file());
        assert_eq!(
            execute(args(["check", project.to_str().unwrap(), "--json"])).exit_code,
            0
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scene_edit_rejects_paths_outside_scene_scope_and_unknown_operation_fields() {
        let directory = test_directory("cli-scene-edit-scope");
        let project = directory.join("project");
        assert_eq!(
            execute(args(["new", project.to_str().unwrap()])).exit_code,
            0
        );
        let error = preview_scene_edit(
            &project,
            "input.json",
            &SceneEditOperation::RenameScene {
                name: "No".to_owned(),
            },
        )
        .unwrap_err();
        assert!(error.contains("limited to project-relative scenes"));

        let invalid = execute(args([
            "edit",
            project.to_str().unwrap(),
            "--file",
            "scenes/first-room.json",
            "--operation-json",
            r#"{"operation":"rename_scene","name":"X","surprise":true}"#,
            "--json",
        ]));
        assert_eq!(invalid.exit_code, 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn screenshot_exports_deterministic_svg_without_overwriting_existing_files() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let directory = test_directory("cli-screenshot");
        let first_path = directory.join("room.svg");
        let second_path = directory.join("room-copy.svg");
        let scene_id = "11000000-0000-4000-8000-000000000001";
        let first = execute(args([
            "screenshot",
            project.to_str().unwrap(),
            "--scene",
            scene_id,
            "--output",
            first_path.to_str().unwrap(),
            "--json",
        ]));
        assert_eq!(first.exit_code, 0, "{}", first.stdout);
        let svg = fs::read_to_string(&first_path).unwrap();
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains("data-entity=\"21000000-0000-4000-8000-000000000001\""));
        assert!(svg.contains("#d6455d"));

        let second = execute(args([
            "screenshot",
            project.to_str().unwrap(),
            "--scene",
            scene_id,
            "--output",
            second_path.to_str().unwrap(),
            "--json",
        ]));
        assert_eq!(second.exit_code, 0, "{}", second.stdout);
        assert_eq!(
            fs::read(&first_path).unwrap(),
            fs::read(&second_path).unwrap()
        );

        let denied = execute(args([
            "screenshot",
            project.to_str().unwrap(),
            "--scene",
            scene_id,
            "--output",
            first_path.to_str().unwrap(),
            "--json",
        ]));
        assert_eq!(denied.exit_code, 3);
        assert_eq!(fs::read_to_string(&first_path).unwrap(), svg);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Keeps the complete transactional CLI round trip visible.
    fn resource_source_edit_previews_hashes_backs_up_and_revalidates() {
        let directory = test_directory("cli-resource-edit");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap(), "--json"]));
        assert_eq!(created.exit_code, 0, "{}", created.stdout);
        fs::write(project.join("assets/old.rgba"), b"old pixels").unwrap();
        fs::write(project.join("assets/new.rgba"), b"new pixels").unwrap();
        let descriptor_path = project.join("assets/texture.hycel.json");
        let original = br#"{
  "schema_version": 1,
  "id": "30000000-0000-4000-8000-000000000001",
  "kind": "texture",
  "source": "assets/old.rgba",
  "import": {}
}
"#;
        fs::write(&descriptor_path, original).unwrap();
        let operation = r#"{"operation":"set_source","source":"assets/new.rgba"}"#;

        let preview = execute(args([
            "resource-edit",
            project.to_str().unwrap(),
            "--file",
            "assets/texture.hycel.json",
            "--operation-json",
            operation,
            "--json",
        ]));
        assert_eq!(preview.exit_code, 0, "{}", preview.stdout);
        assert_eq!(fs::read(&descriptor_path).unwrap(), original);
        let preview: Value = serde_json::from_str(&preview.stdout).unwrap();
        let original_sha256 = preview["result"]["original_sha256"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            preview["result"]["diff"]
                .as_str()
                .unwrap()
                .contains("assets/new.rgba")
        );

        let stale = execute(args([
            "resource-edit",
            project.to_str().unwrap(),
            "--file",
            "assets/texture.hycel.json",
            "--operation-json",
            operation,
            "--apply",
            &"0".repeat(64),
            "--json",
        ]));
        assert_eq!(stale.exit_code, 1);
        assert_eq!(fs::read(&descriptor_path).unwrap(), original);

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let path_digest =
                hycel_assets::ContentHash::from_bytes(b"assets/texture.hycel.json").to_hex();
            let backup_directory = project.join(".hycel/backups");
            fs::create_dir_all(&backup_directory).unwrap();
            let backup_path = backup_directory.join(format!("{path_digest}-{original_sha256}.bak"));
            symlink(&descriptor_path, &backup_path).unwrap();
            let rejected = execute(args([
                "resource-edit",
                project.to_str().unwrap(),
                "--file",
                "assets/texture.hycel.json",
                "--operation-json",
                operation,
                "--apply",
                &original_sha256,
                "--json",
            ]));
            assert_eq!(rejected.exit_code, 1);
            assert!(rejected.stdout.contains("regular, non-symlink file"));
            assert_eq!(fs::read(&descriptor_path).unwrap(), original);
            fs::remove_file(backup_path).unwrap();
        }

        let applied = execute(args([
            "resource-edit",
            project.to_str().unwrap(),
            "--file",
            "assets/texture.hycel.json",
            "--operation-json",
            operation,
            "--apply",
            &original_sha256,
            "--json",
        ]));
        assert_eq!(applied.exit_code, 0, "{}", applied.stdout);
        let applied: Value = serde_json::from_str(&applied.stdout).unwrap();
        assert_eq!(applied["result"]["applied"], true);
        assert!(
            fs::read_to_string(&descriptor_path)
                .unwrap()
                .contains("assets/new.rgba")
        );
        let backup = applied["result"]["backup_path"].as_str().unwrap();
        assert_eq!(fs::read(project.join(backup)).unwrap(), original);
        let edited_bytes = fs::read(&descriptor_path).unwrap();
        let restore_operation = format!(
            "{{\"operation\":\"restore_resource_backup\",\"backup_sha256\":\"{original_sha256}\"}}"
        );
        let current_sha256 = applied["result"]["candidate_sha256"].as_str().unwrap();
        let restored = execute(args([
            "resource-edit",
            project.to_str().unwrap(),
            "--file",
            "assets/texture.hycel.json",
            "--operation-json",
            &restore_operation,
            "--apply",
            current_sha256,
            "--json",
        ]));
        assert_eq!(restored.exit_code, 0, "{}", restored.stdout);
        assert!(
            fs::read_to_string(&descriptor_path)
                .unwrap()
                .contains("assets/old.rgba")
        );
        let restored: Value = serde_json::from_str(&restored.stdout).unwrap();
        let restore_backup = restored["result"]["backup_path"].as_str().unwrap();
        assert_eq!(
            fs::read(project.join(restore_backup)).unwrap(),
            edited_bytes
        );

        let check = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(check.exit_code, 0, "{}", check.stdout);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bellglass_authored_scene_screenshots_match_reviewable_svg_baselines() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/bellglass-courier");
        let directory = test_directory("bellglass-screenshot-baselines");
        let cases = [
            (
                "11000000-0000-4000-8000-000000000001",
                "first-room.svg",
                include_str!("../tests/snapshots/bellglass-courier/first-room.svg"),
            ),
            (
                "11000000-0000-4000-8000-000000000004",
                "middle-room.svg",
                include_str!("../tests/snapshots/bellglass-courier/middle-room.svg"),
            ),
            (
                "11000000-0000-4000-8000-000000000002",
                "last-room.svg",
                include_str!("../tests/snapshots/bellglass-courier/last-room.svg"),
            ),
        ];
        for (scene_id, file_name, expected_svg) in cases {
            let expected_svg = expected_svg.replace("\r\n", "\n");
            let output_path = directory.join(file_name);
            let output = execute(args([
                "screenshot",
                project.to_str().unwrap(),
                "--scene",
                scene_id,
                "--output",
                output_path.to_str().unwrap(),
                "--json",
            ]));
            assert_eq!(output.exit_code, 0, "{}", output.stdout);
            assert_eq!(
                fs::read_to_string(output_path).unwrap(),
                expected_svg,
                "authored-scene SVG baseline changed for {scene_id}"
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn inspect_paginates_scenes_resources_and_dependency_maps_with_stable_offsets() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
        let first = execute(args([
            "inspect",
            project.to_str().unwrap(),
            "--offset",
            "0",
            "--limit",
            "1",
            "--json",
        ]));
        let second = execute(args([
            "inspect",
            project.to_str().unwrap(),
            "--offset",
            "1",
            "--limit",
            "1",
            "--json",
        ]));
        assert_eq!(first.exit_code, 0, "{}", first.stdout);
        assert_eq!(second.exit_code, 0, "{}", second.stdout);
        let first: Value = serde_json::from_str(&first.stdout).unwrap();
        let second: Value = serde_json::from_str(&second.stdout).unwrap();
        assert_eq!(first["result"]["total_scene_count"], 2);
        assert_eq!(first["result"]["next_offset"], 1);
        let maximum_count = first["result"]["total_scene_count"]
            .as_u64()
            .unwrap()
            .max(first["result"]["total_resource_count"].as_u64().unwrap());
        let expected_second_offset = (maximum_count > 2).then_some(Value::from(2));
        assert_eq!(
            second["result"]["next_offset"],
            expected_second_offset.unwrap_or(Value::Null)
        );
        assert_eq!(first["result"]["scenes"].as_array().unwrap().len(), 1);
        assert_eq!(second["result"]["scenes"].as_array().unwrap().len(), 1);
        assert_ne!(
            first["result"]["scenes"][0]["id"],
            second["result"]["scenes"][0]["id"]
        );
        assert_eq!(
            first["result"]["dependencies"]["scenes"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            second["result"]["dependencies"]["scenes"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn example_project_validates_and_reports_animation_dependencies() {
        let project =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/empty-project");
        let checked = execute(args(["check", project.to_str().unwrap(), "--json"]));
        assert_eq!(checked.exit_code, 0, "{}", checked.stdout);
        let inspected = execute(args(["inspect", project.to_str().unwrap(), "--json"]));
        assert_eq!(inspected.exit_code, 0, "{}", inspected.stdout);
        let envelope: Value = serde_json::from_str(&inspected.stdout).unwrap();
        let resources = envelope["result"]["dependencies"]["resources"]
            .as_object()
            .unwrap();
        assert_eq!(resources.len(), 2);
        let animation = &resources["30000000-0000-4000-8000-000000000002"];
        assert_eq!(animation["kind"], "animation");
        assert_eq!(animation["resource_ids"].as_array().unwrap().len(), 1);
        let scene =
            &envelope["result"]["dependencies"]["scenes"]["10000000-0000-4000-8000-000000000001"];
        assert_eq!(scene["resource_ids"].as_array().unwrap().len(), 1);
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
            br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"audio","source":"assets/player.png","import":{}}"#,
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

    #[test]
    fn run_requires_one_explicit_project_path_and_recovery_flag_is_typed() {
        let missing = execute(args(["run", "--json"]));
        assert_eq!(missing.exit_code, 2);
        let envelope: Value = serde_json::from_str(&missing.stdout).unwrap();
        assert_eq!(envelope["command"], "run");
        assert_eq!(envelope["diagnostics"][0]["code"], "HYCEL-CLI-001");

        let unknown = execute(args(["run", ".", "--unknown", "--json"]));
        assert_eq!(unknown.exit_code, 2);
        let envelope: Value = serde_json::from_str(&unknown.stdout).unwrap();
        assert_eq!(envelope["command"], "run");
        assert_eq!(envelope["diagnostics"][0]["code"], "HYCEL-CLI-001");
    }

    #[test]
    fn run_does_not_execute_project_source_for_unsupported_projects() {
        let directory = test_directory("cli-run-no-project-code");
        let project = directory.join("project");
        let created = execute(args(["new", project.to_str().unwrap()]));
        assert_eq!(created.exit_code, 0, "{}", created.stderr);
        let marker = project.join("src/should-not-run.txt");
        fs::write(&marker, b"untouched").unwrap();
        let output = execute(args(["run", project.to_str().unwrap(), "--json"]));
        assert_eq!(output.exit_code, 3);
        let envelope: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(envelope["command"], "run");
        assert!(
            envelope["diagnostics"][0]["code"]
                .as_str()
                .unwrap()
                .starts_with("HYCEL-CLI-120")
        );
        assert_eq!(fs::read(marker).unwrap(), b"untouched");
        fs::remove_dir_all(directory).unwrap();
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
