use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

#[test]
fn native_build_and_package_are_non_overwriting_and_valid_ustar() {
    let temporary = temporary_directory();
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/platformer-game");
    let bundle = temporary.join("bundle");
    let build = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(["build", project.to_str().unwrap(), "--output"])
        .arg(&bundle)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stdout)
    );
    let build_json: Value = serde_json::from_slice(&build.stdout).unwrap();
    assert_eq!(build_json["ok"], true);
    let binary_name = build_json["result"]["executable_name"].as_str().unwrap();
    let game = bundle.join("game");
    assert!(game.join("hycel.toml").is_file());
    assert!(!game.join(".hycel").exists());
    assert!(bundle.join("BUILD.json").is_file());
    let checked = Command::new(bundle.join(binary_name))
        .args(["check", game.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stdout)
    );

    let archive = temporary.join("bundle.tar");
    let packaged = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(["package", bundle.to_str().unwrap(), "--output"])
        .arg(&archive)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        packaged.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged.stdout)
    );
    let packaged_json: Value = serde_json::from_slice(&packaged.stdout).unwrap();
    assert_eq!(
        packaged_json["result"]["format"],
        "application/x-tar; ustar"
    );
    let entries = read_ustar_entries(&archive);
    assert!(entries.iter().any(|entry| entry == "BUILD.json"));
    assert!(entries.iter().any(|entry| entry == "game/hycel.toml"));
    assert!(entries.iter().any(|entry| entry == binary_name));
    assert!(
        entries
            .iter()
            .any(|entry| entry == "run.sh" || entry == "run.cmd")
    );
    let extracted = temporary.join("extracted");
    extract_ustar(&archive, &extracted);
    let extracted_game = extracted.join("game");
    let packaged_tests = Command::new(extracted.join(binary_name))
        .args(["test", extracted_game.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        packaged_tests.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged_tests.stdout)
    );
    let packaged_tests: Value = serde_json::from_slice(&packaged_tests.stdout).unwrap();
    assert_eq!(packaged_tests["result"]["passed_count"], 5);

    let previous_archive = fs::read(&archive).unwrap();
    let repeated = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(["package", bundle.to_str().unwrap(), "--output"])
        .arg(&archive)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!repeated.status.success());
    assert_eq!(fs::read(&archive).unwrap(), previous_archive);
    let repeated_build = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(["build", project.to_str().unwrap(), "--output"])
        .arg(&bundle)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!repeated_build.status.success());
    fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn bellglass_courier_bundle_runs_echo_puzzles_after_clean_extraction() {
    let temporary = temporary_directory();
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bellglass-courier");
    let bundle = temporary.join("bellglass-bundle");
    let build = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(["build", project.to_str().unwrap(), "--output"])
        .arg(&bundle)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stdout)
    );
    let build_json: Value = serde_json::from_slice(&build.stdout).unwrap();
    let binary_name = build_json["result"]["executable_name"].as_str().unwrap();
    let archive = temporary.join("bellglass.tar");
    let packaged = Command::new(env!("CARGO_BIN_EXE_hycel"))
        .args(["package", bundle.to_str().unwrap(), "--output"])
        .arg(&archive)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        packaged.status.success(),
        "{}",
        String::from_utf8_lossy(&packaged.stdout)
    );
    let extracted = temporary.join("bellglass-extracted");
    extract_ustar(&archive, &extracted);
    let tests = Command::new(extracted.join(binary_name))
        .args(["test", extracted.join("game").to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        tests.status.success(),
        "{}",
        String::from_utf8_lossy(&tests.stdout)
    );
    let tests: Value = serde_json::from_slice(&tests.stdout).unwrap();
    assert_eq!(tests["result"]["passed_count"], 6);
    assert_eq!(tests["result"]["failed_count"], 0);
    assert!(extracted.join("game/assets/player-frame-1.rgba").is_file());
    fs::remove_dir_all(temporary).unwrap();
}

fn read_ustar_entries(archive: &Path) -> Vec<String> {
    let mut file = File::open(archive).unwrap();
    let mut entries = Vec::new();
    loop {
        let mut header = [0_u8; 512];
        file.read_exact(&mut header).unwrap();
        if header.iter().all(|byte| *byte == 0) {
            let mut second = [0_u8; 512];
            file.read_exact(&mut second).unwrap();
            assert!(second.iter().all(|byte| *byte == 0));
            assert_eq!(
                file.stream_position().unwrap(),
                file.metadata().unwrap().len()
            );
            return entries;
        }
        let expected_checksum = parse_octal(&header[148..154]);
        let mut checksum_header = header;
        checksum_header[148..156].fill(b' ');
        let checksum = checksum_header
            .iter()
            .map(|byte| u64::from(*byte))
            .sum::<u64>();
        assert_eq!(checksum, expected_checksum);
        assert_eq!(&header[257..263], b"ustar\0");
        let name = nul_terminated(&header[..100]);
        let prefix = nul_terminated(&header[345..500]);
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let size = parse_octal(&header[124..136]);
        entries.push(path);
        let padded = size.saturating_add(511) / 512 * 512;
        file.seek(SeekFrom::Current(i64::try_from(padded).unwrap()))
            .unwrap();
    }
}

fn nul_terminated(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8(bytes[..end].to_vec()).unwrap()
}

fn extract_ustar(archive: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    let root = destination.canonicalize().unwrap();
    let mut file = File::open(archive).unwrap();
    loop {
        let mut header = [0_u8; 512];
        file.read_exact(&mut header).unwrap();
        if header.iter().all(|byte| *byte == 0) {
            let mut second = [0_u8; 512];
            file.read_exact(&mut second).unwrap();
            assert!(second.iter().all(|byte| *byte == 0));
            return;
        }
        let name = nul_terminated(&header[..100]);
        let prefix = nul_terminated(&header[345..500]);
        let name = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let relative = Path::new(&name);
        assert!(
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
        );
        let output = root.join(relative);
        assert!(output.starts_with(&root));
        let mode = parse_octal(&header[100..108]);
        let size = parse_octal(&header[124..136]);
        match header[156] {
            b'5' => fs::create_dir_all(&output).unwrap(),
            0 | b'0' => {
                fs::create_dir_all(output.parent().unwrap()).unwrap();
                let mut output_file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)
                    .unwrap();
                let mut limited = (&mut file).take(size);
                std::io::copy(&mut limited, &mut output_file).unwrap();
                output_file.sync_all().unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        &output,
                        fs::Permissions::from_mode(u32::try_from(mode).unwrap()),
                    )
                    .unwrap();
                }
                #[cfg(not(unix))]
                let _ = mode;
                let padding = size.saturating_add(511) / 512 * 512 - size;
                file.seek(SeekFrom::Current(i64::try_from(padding).unwrap()))
                    .unwrap();
            }
            kind => panic!("unsupported USTAR entry type {kind}"),
        }
    }
}

fn parse_octal(bytes: &[u8]) -> u64 {
    let text = bytes
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .filter(|byte| *byte != b' ')
        .collect::<Vec<_>>();
    u64::from_str_radix(std::str::from_utf8(&text).unwrap(), 8).unwrap()
}

fn temporary_directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "hycel-native-bundle-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}
