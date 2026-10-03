use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

#[test]
fn basic_agent_task_inspects_previews_applies_tests_and_preserves_out_of_scope_files() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
    let project = temporary_directory();
    copy_directory(&source, &project);
    let sentinel_path = project.join("agent-notes.txt");
    fs::write(&sentinel_path, b"outside allowed edit scope\n").unwrap();
    let target_scene = project.join("scenes/first-room.json");
    let target_original = fs::read(&target_scene).unwrap();
    let other_scene = fs::read(project.join("scenes/last-room.json")).unwrap();
    let input = fs::read(project.join("input.json")).unwrap();
    let assets = fs::read_dir(project.join("assets"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect::<Vec<_>>();

    let check = run_cli(["check", project.to_str().unwrap(), "--json"]);
    assert_eq!(check["ok"], true);
    let inspect = run_cli([
        "inspect",
        project.to_str().unwrap(),
        "--offset",
        "0",
        "--limit",
        "1",
        "--json",
    ]);
    assert_eq!(inspect["result"]["limit"], 1);
    assert!(inspect["result"]["next_offset"].is_number());
    let tests = run_cli([
        "test",
        project.to_str().unwrap(),
        "--test",
        "fixed-tick-gameplay",
        "--json",
    ]);
    assert_eq!(tests["ok"], true);

    let operation = json!({"operation":"rename_scene","name":"Agent Reviewed Observatory"});
    let operation_text = serde_json::to_string(&operation).unwrap();
    let preview = run_cli([
        "edit",
        project.to_str().unwrap(),
        "--file",
        "scenes/first-room.json",
        "--operation-json",
        &operation_text,
        "--json",
    ]);
    assert_eq!(preview["ok"], true);
    assert_eq!(fs::read(&target_scene).unwrap(), target_original);
    let original_hash = preview["result"]["original_sha256"].as_str().unwrap();
    let apply = run_cli([
        "edit",
        project.to_str().unwrap(),
        "--file",
        "scenes/first-room.json",
        "--operation-json",
        &operation_text,
        "--apply",
        original_hash,
        "--json",
    ]);
    assert_eq!(apply["ok"], true);
    let backup = project.join(
        apply["result"]["backup_path"]
            .as_str()
            .expect("apply receipt includes backup path"),
    );
    assert_eq!(fs::read(backup).unwrap(), target_original);
    assert!(
        fs::read_to_string(&target_scene)
            .unwrap()
            .contains("Agent Reviewed Observatory")
    );
    assert_eq!(
        fs::read(project.join("scenes/last-room.json")).unwrap(),
        other_scene
    );
    assert_eq!(fs::read(project.join("input.json")).unwrap(), input);

    round_trip_entity_tag_edit(&project, &target_scene);
    round_trip_resource_source_edit(&project);

    assert_eq!(
        fs::read(&sentinel_path).unwrap(),
        b"outside allowed edit scope\n"
    );
    for (name, bytes) in assets {
        assert_eq!(fs::read(project.join("assets").join(name)).unwrap(), bytes);
    }
    assert_eq!(
        run_cli(["check", project.to_str().unwrap(), "--json"])["ok"],
        true
    );
    fs::remove_dir_all(project).unwrap();
}

fn round_trip_entity_tag_edit(project: &Path, scene_path: &Path) {
    let original = fs::read(scene_path).unwrap();
    let operation = json!({
        "operation":"set_entity_tags",
        "entity_id":"21000000-0000-4000-8000-000000000001",
        "tags":["player", "agent-reviewed"]
    });
    let operation_text = serde_json::to_string(&operation).unwrap();
    let preview = run_cli([
        "edit",
        project.to_str().unwrap(),
        "--file",
        "scenes/first-room.json",
        "--operation-json",
        &operation_text,
        "--json",
    ]);
    assert_eq!(preview["ok"], true);
    assert!(
        preview["result"]["diff"]
            .as_str()
            .unwrap()
            .contains("agent-reviewed")
    );
    assert_eq!(fs::read(scene_path).unwrap(), original);
    let source_hash = preview["result"]["original_sha256"].as_str().unwrap();
    let apply = run_cli([
        "edit",
        project.to_str().unwrap(),
        "--file",
        "scenes/first-room.json",
        "--operation-json",
        &operation_text,
        "--apply",
        source_hash,
        "--json",
    ]);
    assert_eq!(apply["ok"], true);
    let backup = project.join(apply["result"]["backup_path"].as_str().unwrap());
    assert_eq!(fs::read(backup).unwrap(), original);
    let scene: Value = serde_json::from_slice(&fs::read(scene_path).unwrap()).unwrap();
    assert_eq!(
        scene["entities"][0]["tags"],
        json!(["player", "agent-reviewed"])
    );
}

fn round_trip_resource_source_edit(project: &Path) {
    let resource_file = "assets/player-frame-1.hycel.json";
    let resource_path = project.join(resource_file);
    let resource_original = fs::read(&resource_path).unwrap();
    let resource_operation = json!({
        "operation":"set_source",
        "source":"assets/player-frame-2.rgba"
    });
    let resource_operation_text = serde_json::to_string(&resource_operation).unwrap();
    let resource_preview = run_cli([
        "resource-edit",
        project.to_str().unwrap(),
        "--file",
        resource_file,
        "--operation-json",
        &resource_operation_text,
        "--json",
    ]);
    assert_eq!(resource_preview["ok"], true);
    assert_eq!(fs::read(&resource_path).unwrap(), resource_original);
    let resource_original_hash = resource_preview["result"]["original_sha256"]
        .as_str()
        .unwrap();
    let resource_apply = run_cli([
        "resource-edit",
        project.to_str().unwrap(),
        "--file",
        resource_file,
        "--operation-json",
        &resource_operation_text,
        "--apply",
        resource_original_hash,
        "--json",
    ]);
    assert_eq!(resource_apply["ok"], true);
    let changed_resource = fs::read(&resource_path).unwrap();
    assert!(String::from_utf8_lossy(&changed_resource).contains("assets/player-frame-2.rgba"));
    let resource_restore = json!({
        "operation":"restore_resource_backup",
        "backup_sha256":resource_original_hash
    });
    let resource_restore_text = serde_json::to_string(&resource_restore).unwrap();
    let restore_preview = run_cli([
        "resource-edit",
        project.to_str().unwrap(),
        "--file",
        resource_file,
        "--operation-json",
        &resource_restore_text,
        "--json",
    ]);
    assert_eq!(restore_preview["ok"], true);
    let current_resource_hash = restore_preview["result"]["original_sha256"]
        .as_str()
        .unwrap();
    let restore_apply = run_cli([
        "resource-edit",
        project.to_str().unwrap(),
        "--file",
        resource_file,
        "--operation-json",
        &resource_restore_text,
        "--apply",
        current_resource_hash,
        "--json",
    ]);
    assert_eq!(restore_apply["ok"], true);
    assert_eq!(fs::read(&resource_path).unwrap(), resource_original);
    assert_eq!(
        fs::read(project.join(restore_apply["result"]["backup_path"].as_str().unwrap())).unwrap(),
        changed_resource
    );
}

fn run_cli<const N: usize>(arguments: [&str; N]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
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
        "hycel-basic-agent-conformance-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}
