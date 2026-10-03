use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

#[test]
fn stdio_subprocess_negotiates_and_serves_bounded_project_tools() {
    let project = temporary_project();
    let mut child = server(&project, false);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let initialize = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"hycel-mcp-stdio-fixture","version":"1"}}}),
    );
    assert_eq!(initialize["result"]["protocolVersion"], "2025-11-25");
    notify(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    let tools = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "project_inspect")
    );
    let inspect = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"project_inspect","arguments":{"offset":0,"limit":1}}}),
    );
    assert_eq!(inspect["result"]["isError"], false);
    assert_eq!(inspect["result"]["structuredContent"]["result"]["limit"], 1);
    let check = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"project_check","_meta":{}}}),
    );
    assert_eq!(check["result"]["isError"], false);
    assert_eq!(check["result"]["structuredContent"]["ok"], true);
    close_process(child, stdin, stdout);
    fs::remove_dir_all(project).unwrap();
}

#[test]
fn advanced_agent_fixture_uses_preview_token_and_preserves_unrelated_files() {
    let project = temporary_project();
    let scene_path = project.join("scenes/first-room.json");
    let original_scene = fs::read(&scene_path).unwrap();
    let manifest = fs::read(project.join("hycel.toml")).unwrap();
    let sentinel_path = project.join("agent-notes.txt");
    fs::write(&sentinel_path, b"outside allowed files\n").unwrap();
    fs::write(project.join("assets/old.rgba"), b"old pixels").unwrap();
    fs::write(project.join("assets/new.rgba"), b"new pixels").unwrap();
    let resource_path = project.join("assets/texture.hycel.json");
    let original_resource = br#"{"schema_version":1,"id":"30000000-0000-4000-8000-000000000001","kind":"texture","source":"assets/old.rgba","import":{}}"#;
    fs::write(&resource_path, original_resource).unwrap();
    let mut child = server(&project, true);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let initialize = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":10,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"advanced-agent-fixture","version":"1"}}}),
    );
    assert_eq!(initialize["result"]["protocolVersion"], "2025-11-25");
    notify(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    let listed = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":11,"method":"tools/list","params":{"_meta":{}}}),
    );
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "scene_edit_apply")
    );
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "resource_edit_apply")
    );
    let operation = json!({"operation":"rename_scene","name":"Advanced Agent Room"});
    let preview = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"scene_edit_preview","arguments":{"scene_file":"scenes/first-room.json","operation":operation}}}),
    );
    assert_eq!(preview["result"]["isError"], false);
    assert_eq!(fs::read(&scene_path).unwrap(), original_scene);
    let preview_result = &preview["result"]["structuredContent"];
    let token = preview_result["preview_token"].as_str().unwrap();
    assert!(preview_result["candidate_sha256"].is_string());
    let apply = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"scene_edit_apply","arguments":{"scene_file":"scenes/first-room.json","operation":operation,"preview_token":token}}}),
    );
    assert_eq!(apply["result"]["isError"], false);
    let backup = project.join(
        apply["result"]["structuredContent"]["backup_path"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(fs::read(backup).unwrap(), original_scene);
    assert!(
        fs::read_to_string(&scene_path)
            .unwrap()
            .contains("Advanced Agent Room")
    );
    assert_eq!(fs::read(project.join("hycel.toml")).unwrap(), manifest);
    assert_eq!(fs::read(sentinel_path).unwrap(), b"outside allowed files\n");
    let check = exchange(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"project_check"}}),
    );
    assert_eq!(check["result"]["isError"], false);
    assert_eq!(check["result"]["structuredContent"]["ok"], true);
    round_trip_entity_create_delete(&mut stdin, &mut stdout, &scene_path);
    round_trip_resource_source_restore(&mut stdin, &mut stdout, &resource_path, original_resource);
    close_process(child, stdin, stdout);
    fs::remove_dir_all(project).unwrap();
}

fn round_trip_entity_create_delete(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    scene_path: &std::path::Path,
) {
    let entity_id = "20000000-0000-4000-8000-000000000001";
    let create_operation =
        json!({"operation":"create_entity","entity_id":entity_id,"name":"Fixture Entity"});
    let create_preview = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"scene_edit_preview","arguments":{"scene_file":"scenes/first-room.json","operation":create_operation}}}),
    );
    let create_token = create_preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .unwrap();
    let create_apply = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":16,"method":"tools/call","params":{"name":"scene_edit_apply","arguments":{"scene_file":"scenes/first-room.json","operation":create_operation,"preview_token":create_token}}}),
    );
    assert_eq!(create_apply["result"]["isError"], false);
    assert!(fs::read_to_string(scene_path).unwrap().contains(entity_id));

    let tag_operation = json!({
        "operation":"set_entity_tags",
        "entity_id":entity_id,
        "tags":["fixture", "reviewed"]
    });
    let tag_preview = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"scene_edit_preview","arguments":{"scene_file":"scenes/first-room.json","operation":tag_operation}}}),
    );
    assert_eq!(tag_preview["result"]["isError"], false);
    let tag_token = tag_preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .unwrap();
    let tag_apply = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":18,"method":"tools/call","params":{"name":"scene_edit_apply","arguments":{"scene_file":"scenes/first-room.json","operation":tag_operation,"preview_token":tag_token}}}),
    );
    assert_eq!(tag_apply["result"]["isError"], false);
    let scene: Value = serde_json::from_slice(&fs::read(scene_path).unwrap()).unwrap();
    assert_eq!(scene["entities"][0]["tags"], json!(["fixture", "reviewed"]));

    let delete_operation = json!({"operation":"delete_entity","entity_id":entity_id});
    let delete_preview = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":19,"method":"tools/call","params":{"name":"scene_edit_preview","arguments":{"scene_file":"scenes/first-room.json","operation":delete_operation}}}),
    );
    let delete_token = delete_preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .unwrap();
    let delete_apply = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"scene_edit_apply","arguments":{"scene_file":"scenes/first-room.json","operation":delete_operation,"preview_token":delete_token}}}),
    );
    assert_eq!(delete_apply["result"]["isError"], false);
    assert!(!fs::read_to_string(scene_path).unwrap().contains(entity_id));
}

fn round_trip_resource_source_restore(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    resource_path: &std::path::Path,
    original: &[u8],
) {
    let resource_file = "assets/texture.hycel.json";
    let operation = json!({"operation":"set_source","source":"assets/new.rgba"});
    let preview = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"resource_edit_preview","arguments":{"resource_file":resource_file,"operation":operation}}}),
    );
    assert_eq!(preview["result"]["isError"], false);
    assert_eq!(fs::read(resource_path).unwrap(), original);
    let token = preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .unwrap();
    let original_sha256 = preview["result"]["structuredContent"]["original_sha256"]
        .as_str()
        .unwrap();
    let apply = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"resource_edit_apply","arguments":{"resource_file":resource_file,"operation":operation,"preview_token":token}}}),
    );
    assert_eq!(apply["result"]["isError"], false);
    let changed = fs::read(resource_path).unwrap();
    assert!(String::from_utf8_lossy(&changed).contains("assets/new.rgba"));
    let backup = apply["result"]["structuredContent"]["backup_path"]
        .as_str()
        .unwrap();
    assert_eq!(
        fs::read(
            resource_path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(backup)
        )
        .unwrap(),
        original
    );

    let restore = json!({"operation":"restore_resource_backup","backup_sha256":original_sha256});
    let restore_preview = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":23,"method":"tools/call","params":{"name":"resource_edit_preview","arguments":{"resource_file":resource_file,"operation":restore}}}),
    );
    assert_eq!(restore_preview["result"]["isError"], false);
    let restore_token = restore_preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .unwrap();
    let restore_apply = exchange(
        stdin,
        stdout,
        json!({"jsonrpc":"2.0","id":24,"method":"tools/call","params":{"name":"resource_edit_apply","arguments":{"resource_file":resource_file,"operation":restore,"preview_token":restore_token}}}),
    );
    assert_eq!(restore_apply["result"]["isError"], false);
    assert_eq!(fs::read(resource_path).unwrap(), original);
    let current_backup = restore_apply["result"]["structuredContent"]["backup_path"]
        .as_str()
        .unwrap();
    assert_eq!(
        fs::read(
            resource_path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(current_backup)
        )
        .unwrap(),
        changed
    );
}

fn server(project: &std::path::Path, allow_writes: bool) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hycel-mcp"));
    command
        .arg("--project-root")
        .arg(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if allow_writes {
        command.arg("--allow-writes");
    }
    command.spawn().unwrap()
}

#[allow(clippy::needless_pass_by_value)]
fn exchange(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>, value: Value) -> Value {
    serde_json::to_writer(&mut *stdin, &value).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    assert_ne!(
        stdout.read_line(&mut line).unwrap(),
        0,
        "server closed before responding"
    );
    serde_json::from_str(&line).unwrap()
}

#[allow(clippy::needless_pass_by_value)]
fn notify(stdin: &mut ChildStdin, value: Value) {
    serde_json::to_writer(&mut *stdin, &value).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn close_process(mut child: Child, stdin: ChildStdin, mut stdout: BufReader<ChildStdout>) {
    drop(stdin);
    let mut remainder = Vec::new();
    stdout.read_to_end(&mut remainder).unwrap();
    assert!(remainder.is_empty());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(child.wait().unwrap().success(), "{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn temporary_project() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let project = std::env::temp_dir().join(format!(
        "hycel-mcp-stdio-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let created = hycel_cli::execute(["new", project.to_str().unwrap(), "--json"].map(Into::into));
    assert_eq!(created.exit_code, 0, "{}", created.stdout);
    project
}
