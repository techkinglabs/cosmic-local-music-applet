use rusqlite::Connection;
use std::path::{Path, PathBuf};

const MUSIC_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav", "m4a", "opus", "wma"];

#[derive(Debug, Clone)]
pub struct MusicStats {
    pub track_count: usize,
    pub album_count: usize,
    pub last_scanned: u64,
}

pub struct MusicStatsDb {
    conn: Connection,
}

fn data_home() -> PathBuf {
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
        if !xdg_data.is_empty() {
            return PathBuf::from(xdg_data);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".local/share");
        }
    }
    PathBuf::from("/tmp")
}

fn _db_path() -> PathBuf {
    data_home().join("cosmic-media-applet").join("music_stats.db")
}

fn music_folder() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let user_dirs = PathBuf::from(&home).join(".config/user-dirs.dirs");
            if let Ok(content) = std::fs::read_to_string(&user_dirs) {
                for line in content.lines() {
                    if let Some(rest) = line.strip_prefix("XDG_MUSIC_DIR=") {
                        let trimmed = rest.trim().trim_matches('"');
                        if let Some(expanded) = trimmed.strip_prefix("$HOME") {
                            if !expanded.is_empty() {
                                let stripped = expanded.strip_prefix('/').unwrap_or(expanded);
                                if !stripped.is_empty() {
                                    return PathBuf::from(&home).join(stripped);
                                }
                            }
                            return PathBuf::from(&home).join("Music");
                        }
                        return PathBuf::from(trimmed);
                    }
                }
            }
            return PathBuf::from(&home).join("Music");
        }
    }
    PathBuf::from("/tmp/Music")
}

impl MusicStatsDb {
    pub fn new() -> anyhow::Result<Self> {
        let path = Self::db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS music_stats (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                track_count INTEGER NOT NULL DEFAULT 0,
                album_count INTEGER NOT NULL DEFAULT 0,
                last_scanned INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO music_stats (id, track_count, album_count, last_scanned) VALUES (1, 0, 0, 0)",
            [],
        )?;
        let has_album = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('music_stats') WHERE name = 'album_count'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) > 0;
        if !has_album {
            conn.execute("ALTER TABLE music_stats ADD COLUMN album_count INTEGER NOT NULL DEFAULT 0", [])?;
        }
        Ok(Self { conn })
    }

    pub fn get_stats(&self) -> (usize, usize, u64) {
        match self.conn.query_row(
            "SELECT track_count, album_count, last_scanned FROM music_stats WHERE id = 1",
            [],
            |row| {
                let count: i64 = row.get(0)?;
                let albums: i64 = row.get(1)?;
                let ts: i64 = row.get(2)?;
                Ok((count as usize, albums as usize, ts as u64))
            },
        ) {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read stats from DB; returning defaults");
                (0, 0, 0)
            }
        }
    }

    pub fn set_stats(&self, track_count: usize, album_count: usize, timestamp: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE music_stats SET track_count = ?, album_count = ?, last_scanned = ? WHERE id = 1",
            rusqlite::params![track_count as i64, album_count as i64, timestamp as i64],
        )?;
        Ok(())
    }

    pub fn db_path() -> PathBuf {
        _db_path()
    }

    pub fn scan_music_folder(dir: &Path) -> (usize, usize) {
        if !dir.exists() {
            tracing::warn!(dir = ?dir, "Music folder does not exist");
            return (0, 0);
        }
        let mut track_count = 0usize;
        let mut album_count = 0usize;
        let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            let entries = match std::fs::read_dir(&current) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(dir = ?current, error = %e, "Failed to read directory");
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to read directory entry");
                        continue;
                    }
                };
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    album_count += 1;
                } else if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if MUSIC_EXTENSIONS
                            .iter()
                            .any(|&e| e.eq_ignore_ascii_case(ext.to_str().unwrap_or("")))
                        {
                            track_count += 1;
                        }
                    }
                }
            }
        }
        (track_count, album_count)
    }

    pub fn music_folder() -> PathBuf {
        music_folder()
    }
}

impl Default for MusicStatsDb {
    fn default() -> Self {
        Self::new().expect("Failed to initialize MusicStatsDb")
    }
}

#[cfg(test)]
mod tests;
