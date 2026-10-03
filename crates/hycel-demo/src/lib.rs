//! Compiled-in reference games used by the project CLI and examples.
//!
//! The runtime supports the legacy two-room platformer and The Bellglass Courier.
//! Game logic remains Hycel-owned Rust code; callers select project data explicitly,
//! and project source remains inert.

#[allow(dead_code)]
#[path = "../examples/playable_platformer.rs"]
mod playable_platformer;

pub use playable_platformer::{ReplayOutcome, ScenarioOutcome};

/// Opens the compiled-in reference runtime on one explicit project directory.
///
/// This only runs Hycel's compiled-in reference game; it does not compile or
/// execute project-provided source code.
///
/// # Errors
///
/// Returns an error if authored game data, save recovery, platform setup, or
/// presentation fails.
pub fn run_playable_platformer(
    project_root: &std::path::Path,
    recover_save: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    playable_platformer::run_project(project_root, recover_save)
}

/// Runs headless named checks against the compiled-in reference game.
///
/// # Errors
///
/// Returns an error when project data is not compatible with the reference
/// game, or when the requested scenario name is unknown.
pub fn test_playable_platformer(
    project_root: &std::path::Path,
    selected: Option<&str>,
) -> Result<Vec<ScenarioOutcome>, Box<dyn std::error::Error>> {
    playable_platformer::run_scenarios(project_root, selected)
}

/// Replays a bounded tick-indexed input file through the reference game.
///
/// # Errors
///
/// Returns an error for invalid/incompatible replay metadata, incompatible
/// project data, or a failed simulation tick.
pub fn replay_playable_platformer(
    project_root: &std::path::Path,
    replay_bytes: &[u8],
) -> Result<ReplayOutcome, Box<dyn std::error::Error>> {
    playable_platformer::replay_project(project_root, replay_bytes)
}
