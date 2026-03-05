use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseState {
    pub status: PhaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssignment {
    pub reviewer_id: u32,
    pub starting_file: String,
    pub scout_score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub started_at: String,
    pub target: PathBuf,
    pub seed: u64,
    pub phases: HashMap<String, PhaseState>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub reviewer_count: u32,
    #[serde(default)]
    pub task_assignments: Vec<TaskAssignment>,
}

pub const PHASE_ORDER: &[&str] = &[
    "exploration",
    "scouting",
    "deep_review",
    "appraisal_compilation",
];

impl PhaseState {
    pub fn pending() -> Self {
        Self {
            status: PhaseStatus::Pending,
            session_id: None,
            cost_usd: None,
            reason: None,
        }
    }
}

impl State {
    pub fn new(target: PathBuf, seed: u64) -> Self {
        let mut phases = HashMap::new();
        for name in PHASE_ORDER {
            phases.insert(name.to_string(), PhaseState::pending());
        }
        Self {
            version: 1,
            started_at: epoch_timestamp(),
            target,
            seed,
            phases,
            files: Vec::new(),
            reviewer_count: 0,
            task_assignments: Vec::new(),
        }
    }

    pub fn path(target: &Path) -> PathBuf {
        target.join(".kuriboh").join("state.json")
    }

    pub fn load(target: &Path) -> Result<Self> {
        let path = Self::path(target);
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let state: Self =
            serde_json::from_str(&data).with_context(|| format!("parsing {}", path.display()))?;
        Ok(state)
    }

    pub fn save(&self, target: &Path) -> Result<()> {
        let path = Self::path(target);
        let dir = path.parent().expect(".kuriboh dir");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("state.json.tmp");
        let data = serde_json::to_string_pretty(self).context("serializing state")?;
        std::fs::write(&tmp, &data)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    pub fn phase_mut(&mut self, name: &str) -> &mut PhaseState {
        self.phases.get_mut(name).expect("unknown phase name")
    }

    pub fn phase_status(&self, name: &str) -> &PhaseStatus {
        &self.phases[name].status
    }
}

fn epoch_timestamp() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", dur.as_secs())
}

/// Check whether a phase's output sentinel is satisfied.
pub fn check_sentinel(target: &Path, phase: &str, state: &State) -> Result<bool> {
    let kb = target.join(".kuriboh");
    match phase {
        "exploration" => {
            let path = kb.join("exploration.md");
            match std::fs::metadata(&path) {
                Ok(m) => Ok(m.len() > 100),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).context("checking exploration sentinel"),
            }
        }
        "scouting" => {
            let path = kb.join("scores.json");
            match std::fs::read_to_string(&path) {
                Ok(data) => {
                    let v: Result<serde_json::Value, _> = serde_json::from_str(&data);
                    Ok(v.is_ok())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).context("checking scouting sentinel"),
            }
        }
        "deep_review" => {
            if state.task_assignments.is_empty() {
                return Ok(false);
            }
            for a in &state.task_assignments {
                let path = kb.join(format!("findings/reviewer-{}.json", a.reviewer_id));
                if !path.exists() {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "appraisal_compilation" => {
            let path = kb.join("compiled-findings.json");
            match std::fs::read_to_string(&path) {
                Ok(data) => {
                    let v: Result<serde_json::Value, _> = serde_json::from_str(&data);
                    Ok(v.is_ok())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e).context("checking compilation sentinel"),
            }
        }
        _ => bail!("unknown phase: {phase}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_has_all_phases_pending() {
        let state = State::new(PathBuf::from("/tmp/test"), 42);
        for name in PHASE_ORDER {
            assert_eq!(state.phase_status(name), &PhaseStatus::Pending);
        }
    }

    #[test]
    fn round_trip_serialization() {
        let mut state = State::new(PathBuf::from("/tmp/test"), 42);
        state.files = vec!["src/main.rs".to_string()];
        state.task_assignments = vec![TaskAssignment {
            reviewer_id: 1,
            starting_file: "src/main.rs".to_string(),
            scout_score: 75,
        }];
        state.phase_mut("exploration").status = PhaseStatus::Done;
        state.phase_mut("exploration").cost_usd = Some(0.15);

        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: State = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.seed, 42);
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.task_assignments.len(), 1);
        assert_eq!(loaded.phase_status("exploration"), &PhaseStatus::Done);
        assert_eq!(loaded.phase_status("scouting"), &PhaseStatus::Pending);
    }

    #[test]
    fn atomic_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path();
        let state = State::new(target.to_path_buf(), 123);
        state.save(target).unwrap();
        let loaded = State::load(target).unwrap();
        assert_eq!(loaded.seed, 123);
        assert_eq!(loaded.version, 1);
    }

    #[test]
    fn sentinel_exploration_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().to_path_buf(), 1);
        assert!(!check_sentinel(dir.path(), "exploration", &state).unwrap());
    }

    #[test]
    fn sentinel_exploration_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh");
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::write(kb.join("exploration.md"), "short").unwrap();
        let state = State::new(dir.path().to_path_buf(), 1);
        assert!(!check_sentinel(dir.path(), "exploration", &state).unwrap());
    }

    #[test]
    fn sentinel_exploration_passes() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh");
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::write(kb.join("exploration.md"), "x".repeat(200)).unwrap();
        let state = State::new(dir.path().to_path_buf(), 1);
        assert!(check_sentinel(dir.path(), "exploration", &state).unwrap());
    }

    #[test]
    fn sentinel_deep_review_checks_all_reviewers() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join(".kuriboh/findings");
        std::fs::create_dir_all(&kb).unwrap();

        let mut state = State::new(dir.path().to_path_buf(), 1);
        state.task_assignments = vec![
            TaskAssignment {
                reviewer_id: 1,
                starting_file: "a.rs".into(),
                scout_score: 50,
            },
            TaskAssignment {
                reviewer_id: 2,
                starting_file: "b.rs".into(),
                scout_score: 60,
            },
        ];

        std::fs::write(kb.join("reviewer-1.json"), "[]").unwrap();
        assert!(!check_sentinel(dir.path(), "deep_review", &state).unwrap());

        std::fs::write(kb.join("reviewer-2.json"), "[]").unwrap();
        assert!(check_sentinel(dir.path(), "deep_review", &state).unwrap());
    }
}
