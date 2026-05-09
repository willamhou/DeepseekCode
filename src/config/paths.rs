use std::path::PathBuf;

use super::types::WorkspaceConfig;

impl WorkspaceConfig {
    pub fn benchmark_manifest_path(&self) -> PathBuf {
        PathBuf::from(".dscode").join("benchmarks.txt")
    }

    pub fn config_path(&self) -> PathBuf {
        PathBuf::from(&self.config_dir).join("config.toml")
    }

    pub fn session_dir(&self) -> PathBuf {
        PathBuf::from(&self.session_dir)
    }

    pub fn commands_dir(&self) -> PathBuf {
        PathBuf::from(&self.config_dir).join("commands")
    }

    pub fn user_commands_dir(&self) -> PathBuf {
        crate::skills::tilde::expand_tilde(&self.user_commands_dir)
    }

    pub fn user_instructions_file(&self) -> Option<PathBuf> {
        let path = self.user_instructions_file.trim();
        if path.is_empty() {
            None
        } else {
            Some(crate::skills::tilde::expand_tilde(path))
        }
    }

    pub fn dogfood_dir(&self) -> PathBuf {
        PathBuf::from(&self.config_dir).join("dogfood")
    }

    pub fn dogfood_ledger_path(&self) -> PathBuf {
        self.dogfood_dir().join("ledger.jsonl")
    }

    pub fn dogfood_report_path(&self) -> PathBuf {
        self.dogfood_dir().join("latest.md")
    }

    pub fn dogfood_benchmark_seed_path(&self) -> PathBuf {
        self.dogfood_dir().join("benchmark-seeds.txt")
    }

    pub fn benchmark_dir(&self) -> PathBuf {
        PathBuf::from(".dscode").join("benchmarks")
    }

    pub fn benchmark_history_path(&self) -> PathBuf {
        self.benchmark_dir().join("history.jsonl")
    }
}
