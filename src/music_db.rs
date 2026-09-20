use rodio::Source;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const MUSIC_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav", "m4a", "opus", "wma"];

#[derive(Debug, Clone)]
pub struct MusicStats {
    pub track_count: usize,
    pub album_count: usize,
    pub last_scanned: u64,
}

#[derive(Debug, Clone)]
pub struct TrackStat {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub play_count: u64,
    pub is_favorite: bool,
    pub last_played: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AlbumInfo {
    pub album: String,
    pub artist: String,
    pub directory: String,
    pub track_count: usize,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortMode {
    NameAsc,
    NameDesc,
    PlayCountAsc,
    PlayCountDesc,
}

pub fn music_folder() -> PathBuf {
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

fn extract_tags(path: &Path) -> (String, String, String, u64) {
    let mut title = String::new();
    let artist = String::new();
    let mut album = String::new();

    let duration_ms = std::fs::File::open(path)
        .ok()
        .and_then(|f| rodio::Decoder::new(std::io::BufReader::new(f)).ok())
        .map(|src| {
            let dur = src.total_duration();
            dur.map(|d| d.as_millis() as u64).unwrap_or(0)
        })
        .unwrap_or_else(|| {
            std::fs::File::open(path)
                .ok()
                .and_then(|f| {
                    let mut decoder = minimp3::Decoder::new(std::io::BufReader::new(f));
                    let mut total_samples = 0usize;
                    let mut sample_rate = 0u32;
                    let mut channels = 0u16;
                    while let Ok(frame) = decoder.next_frame() {
                        sample_rate = frame.sample_rate as u32;
                        channels = frame.channels as u16;
                        total_samples += frame.data.len();
                    }
                    if total_samples > 0 && sample_rate > 0 && channels > 0 {
                        Some((total_samples as f64 / sample_rate as f64 / channels as f64 * 1000.0) as u64)
                    } else {
                        None
                    }
                })
                .unwrap_or(0)
        });

    if title.is_empty() {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            title = stem.to_string();
        }
    }
    if album.is_empty() {
        if let Some(parent) = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
            album = parent.to_string();
        }
    }

    (title, artist, album, duration_ms)
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

        conn.execute(
            "CREATE TABLE IF NOT EXISTS tracks (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                artist TEXT NOT NULL DEFAULT '',
                album TEXT NOT NULL,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                play_count INTEGER NOT NULL DEFAULT 0,
                is_favorite INTEGER NOT NULL DEFAULT 0,
                last_played INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_play_count ON tracks(play_count DESC)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_favorite ON tracks(is_favorite DESC, play_count DESC)",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS playback_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                last_path TEXT,
                last_position_ms INTEGER NOT NULL DEFAULT 0,
                last_state TEXT NOT NULL DEFAULT 'stopped'
            )",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO playback_state (id, last_path, last_position_ms, last_state) VALUES (1, NULL, 0, 'stopped')",
            [],
        )?;

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

        let db_path = Self::db_path();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let conn = match Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "Failed to open DB for track scan");
                return (0, 0);
            }
        };

        conn.execute(
            "CREATE TABLE IF NOT EXISTS tracks (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                artist TEXT NOT NULL DEFAULT '',
                album TEXT NOT NULL,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                play_count INTEGER NOT NULL DEFAULT 0,
                is_favorite INTEGER NOT NULL DEFAULT 0,
                last_played INTEGER NOT NULL DEFAULT 0
            )",
            [],
        ).ok();

        let existing: std::collections::HashSet<String> = {
            match conn.prepare("SELECT path FROM tracks") {
                Ok(mut stmt) => {
                    match stmt.query_map([], |row| row.get::<_, String>(0)) {
                        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to read existing track paths");
                            std::collections::HashSet::new()
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to prepare existing track query");
                    std::collections::HashSet::new()
                }
            }
        };

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
                            let path_str = path.to_string_lossy().to_string();
                            if !existing.contains(&path_str) {
                                let (title, artist, album, duration_ms) = extract_tags(&path);
                                if conn.execute(
                                    "INSERT OR REPLACE INTO tracks (path, title, artist, album, duration_ms, play_count, is_favorite, last_played)
                                     VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, 0)",
                                    rusqlite::params![&path_str, &title, &artist, &album, duration_ms as i64],
                                ).is_ok() {
                                    track_count += 1;
                                }
                            } else {
                                track_count += 1;
                            }
                        }
                    }
                }
            }
        }

        (track_count, album_count)
    }

    pub fn get_tracks_sorted_by_play_count(&self) -> anyhow::Result<Vec<TrackStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
             FROM tracks ORDER BY play_count DESC, title ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TrackStat {
                path: row.get::<_, String>(0)?,
                title: row.get::<_, String>(1)?,
                artist: row.get::<_, String>(2)?,
                album: row.get::<_, String>(3)?,
                duration_ms: row.get::<_, i64>(4)? as u64,
                play_count: row.get::<_, i64>(5)? as u64,
                is_favorite: row.get::<_, i64>(6)? != 0,
                last_played: row.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut tracks = Vec::new();
        for row in rows {
            tracks.push(row?);
        }
        Ok(tracks)
    }

    pub fn get_tracks_sorted(&self, sort_mode: SortMode) -> anyhow::Result<Vec<TrackStat>> {
        let order = match sort_mode {
            SortMode::NameAsc => "ORDER BY title ASC",
            SortMode::NameDesc => "ORDER BY title DESC",
            SortMode::PlayCountAsc => "ORDER BY play_count ASC, title ASC",
            SortMode::PlayCountDesc => "ORDER BY play_count DESC, title ASC",
        };
        let sql = format!(
            "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
             FROM tracks {}",
            order
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(TrackStat {
                path: row.get::<_, String>(0)?,
                title: row.get::<_, String>(1)?,
                artist: row.get::<_, String>(2)?,
                album: row.get::<_, String>(3)?,
                duration_ms: row.get::<_, i64>(4)? as u64,
                play_count: row.get::<_, i64>(5)? as u64,
                is_favorite: row.get::<_, i64>(6)? != 0,
                last_played: row.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut tracks = Vec::new();
        for row in rows {
            tracks.push(row?);
        }
        Ok(tracks)
    }

    pub fn search_tracks(&self, query: &str) -> anyhow::Result<Vec<TrackStat>> {
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = self.conn.prepare(
            "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
             FROM tracks
             WHERE lower(title) LIKE ?1 OR lower(artist) LIKE ?1 OR lower(album) LIKE ?1
             ORDER BY play_count DESC, title ASC"
        )?;
        let rows = stmt.query_map([&pattern], |row| {
            Ok(TrackStat {
                path: row.get::<_, String>(0)?,
                title: row.get::<_, String>(1)?,
                artist: row.get::<_, String>(2)?,
                album: row.get::<_, String>(3)?,
                duration_ms: row.get::<_, i64>(4)? as u64,
                play_count: row.get::<_, i64>(5)? as u64,
                is_favorite: row.get::<_, i64>(6)? != 0,
                last_played: row.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut tracks = Vec::new();
        for row in rows {
            tracks.push(row?);
        }
        Ok(tracks)
    }

    pub fn get_albums(&self) -> anyhow::Result<Vec<AlbumInfo>> {
        self.get_albums_sorted(SortMode::NameAsc)
    }

    pub fn get_albums_sorted(&self, sort_mode: SortMode) -> anyhow::Result<Vec<AlbumInfo>> {
        let order = match sort_mode {
            SortMode::NameAsc => "ORDER BY album ASC",
            SortMode::NameDesc => "ORDER BY album DESC",
            SortMode::PlayCountAsc => "ORDER BY total_play_count ASC, album ASC",
            SortMode::PlayCountDesc => "ORDER BY total_play_count DESC, album ASC",
        };
        let sql = format!(
            "SELECT album, artist, COUNT(*) as cnt, COALESCE(SUM(play_count), 0) as total_play_count
             FROM tracks
             GROUP BY album, artist
             {}",
            order
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(AlbumInfo {
                album: row.get::<_, String>(0)?,
                artist: row.get::<_, String>(1)?,
                directory: row.get::<_, String>(0)?,
                track_count: row.get::<_, i64>(2)? as usize,
            })
        })?;
        let mut albums = Vec::new();
        for row in rows {
            albums.push(row?);
        }
        Ok(albums)
    }

    pub fn get_tracks_in_album(&self, album: &str) -> anyhow::Result<Vec<TrackStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
             FROM tracks
             WHERE album = ?1
             ORDER BY title ASC"
        )?;
        let rows = stmt.query_map([album], |row| {
            Ok(TrackStat {
                path: row.get::<_, String>(0)?,
                title: row.get::<_, String>(1)?,
                artist: row.get::<_, String>(2)?,
                album: row.get::<_, String>(3)?,
                duration_ms: row.get::<_, i64>(4)? as u64,
                play_count: row.get::<_, i64>(5)? as u64,
                is_favorite: row.get::<_, i64>(6)? != 0,
                last_played: row.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut tracks = Vec::new();
        for row in rows {
            tracks.push(row?);
        }
        Ok(tracks)
    }

    pub fn get_favorite_tracks(&self) -> anyhow::Result<Vec<TrackStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
             FROM tracks
             WHERE is_favorite = 1
             ORDER BY play_count DESC, title ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TrackStat {
                path: row.get::<_, String>(0)?,
                title: row.get::<_, String>(1)?,
                artist: row.get::<_, String>(2)?,
                album: row.get::<_, String>(3)?,
                duration_ms: row.get::<_, i64>(4)? as u64,
                play_count: row.get::<_, i64>(5)? as u64,
                is_favorite: row.get::<_, i64>(6)? != 0,
                last_played: row.get::<_, i64>(7)? as u64,
            })
        })?;
        let mut tracks = Vec::new();
        for row in rows {
            tracks.push(row?);
        }
        Ok(tracks)
    }

    pub fn toggle_favorite(&self, path: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE tracks SET is_favorite = 1 - is_favorite WHERE path = ?1",
            rusqlite::params![path],
        )?;
        Ok(())
    }

    pub fn delete_track(&self, path: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM tracks WHERE path = ?1",
            rusqlite::params![path],
        )?;
        Ok(())
    }

    pub fn increment_play_count(&self, path: &str) -> anyhow::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.conn.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played = ?2 WHERE path = ?1",
            rusqlite::params![path, now as i64],
        )?;
        Ok(())
    }

    pub fn get_track_by_path(&self, path: &str) -> anyhow::Result<Option<TrackStat>> {
        self.conn.query_row(
            "SELECT path, title, artist, album, duration_ms, play_count, is_favorite, last_played
             FROM tracks WHERE path = ?1",
            rusqlite::params![path],
            |row| {
                Ok(TrackStat {
                    path: row.get::<_, String>(0)?,
                    title: row.get::<_, String>(1)?,
                    artist: row.get::<_, String>(2)?,
                    album: row.get::<_, String>(3)?,
                    duration_ms: row.get::<_, i64>(4)? as u64,
                    play_count: row.get::<_, i64>(5)? as u64,
                    is_favorite: row.get::<_, i64>(6)? != 0,
                    last_played: row.get::<_, i64>(7)? as u64,
                })
            },
        ).map(Some).or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(anyhow::anyhow!(e)),
        })
    }

    pub fn save_playback_state(&self, path: Option<&str>, position_ms: u64, state: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO playback_state (id, last_path, last_position_ms, last_state) VALUES (1, ?1, ?2, ?3)",
            rusqlite::params![path, position_ms as i64, state],
        )?;
        Ok(())
    }

    pub fn get_playback_state(&self) -> Option<(Option<String>, u64, String)> {
        let row = self.conn.query_row(
            "SELECT last_path, last_position_ms, last_state FROM playback_state WHERE id = 1",
            [],
            |row| {
                let path: Option<String> = row.get(0)?;
                let pos: i64 = row.get(1)?;
                let state: String = row.get(2)?;
                Ok((path, pos as u64, state))
            },
        ).ok()?;
        Some(row)
    }
}

impl Default for MusicStatsDb {
    fn default() -> Self {
        Self::new().expect("Failed to initialize MusicStatsDb")
    }
}

#[cfg(test)]
mod tests;
