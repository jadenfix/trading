use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};

pub const BOT_TRADE_DIR: &str = "llm-rules-bot";

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn resolve_repo_root() -> Option<PathBuf> {
    let mut cursor = std::env::current_dir().ok()?;
    loop {
        if cursor.join(".git").is_dir() {
            return Some(cursor);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

pub fn resolve_trades_dir() -> PathBuf {
    if let Ok(raw) = std::env::var("TRADES_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(BOT_TRADE_DIR);
        }
    }
    if let Some(root) = resolve_repo_root() {
        return root.join("TRADES").join(BOT_TRADE_DIR);
    }
    PathBuf::from("TRADES").join(BOT_TRADE_DIR)
}

pub struct TradeJournal {
    dir: PathBuf,
    day_key: String,
    file: File,
}

impl TradeJournal {
    pub fn open(dir: PathBuf) -> std::io::Result<Self> {
        create_dir_all(&dir)?;
        let day_key = Utc::now().format("%Y-%m-%d").to_string();
        let file = Self::open_day_file(&dir, &day_key)?;
        Ok(Self { dir, day_key, file })
    }

    fn open_day_file(dir: &Path, day_key: &str) -> std::io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("trades-{}.jsonl", day_key)))
    }

    fn rotate_if_needed(&mut self) -> std::io::Result<()> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if today != self.day_key {
            self.file = Self::open_day_file(&self.dir, &today)?;
            self.day_key = today;
        }
        Ok(())
    }

    pub fn write_event(&mut self, event: serde_json::Value) {
        let result = (|| -> std::io::Result<()> {
            self.rotate_if_needed()?;
            let line = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
            writeln!(self.file, "{}", line)?;
            self.file.flush()?;
            Ok(())
        })();

        if let Err(e) = result {
            tracing::warn!("journal write failed: {}", e);
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
