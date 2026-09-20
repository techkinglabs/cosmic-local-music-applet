use std::fs;
use std::path::PathBuf;
use crate::music_db::MusicStatsDb;

fn temp_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cosmic-media-applet-test-{}-{}",
        std::process::id(),
        test_name
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_file(dir: &PathBuf, name: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, b"test").unwrap();
}

#[test]
fn test_scan_empty_directory() {
    let dir = temp_dir("empty");
    let empty_subdir = dir.join("empty");
    fs::create_dir_all(&empty_subdir).unwrap();
    let (tracks, albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 0);
    assert_eq!(albums, 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_music_files() {
    let dir = temp_dir("music_files");
    create_file(&dir, "song1.mp3");
    create_file(&dir, "song2.flac");
    create_file(&dir, "song3.ogg");
    create_file(&dir, "song4.wav");
    create_file(&dir, "song5.m4a");
    create_file(&dir, "song6.opus");
    create_file(&dir, "song7.wma");
    let (tracks, _albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 7);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_filters_non_music_files() {
    let dir = temp_dir("filters_non_music");
    create_file(&dir, "song1.mp3");
    create_file(&dir, "readme.txt");
    create_file(&dir, "image.jpg");
    create_file(&dir, "video.mp4");
    let (tracks, _albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_skips_hidden_files() {
    let dir = temp_dir("skips_hidden_files");
    create_file(&dir, "song1.mp3");
    create_file(&dir, ".hidden_song.mp3");
    let (tracks, _albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_skips_hidden_directories() {
    let dir = temp_dir("skips_hidden_dirs");
    create_file(&dir, "song1.mp3");
    create_file(&dir, ".hidden_dir/song2.mp3");
    create_file(&dir, "visible_dir/song3.mp3");
    let (tracks, _albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_nested_directories() {
    let dir = temp_dir("nested_dirs");
    create_file(&dir, "song1.mp3");
    create_file(&dir, "album1/song2.flac");
    create_file(&dir, "album1/subalbum/song3.ogg");
    create_file(&dir, "album2/song4.wav");
    let (tracks, albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 4);
    assert_eq!(albums, 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_case_insensitive_extensions() {
    let dir = temp_dir("case_insensitive");
    create_file(&dir, "SONG1.MP3");
    create_file(&dir, "Song2.FLAC");
    create_file(&dir, "song3.Ogg");
    let (tracks, _albums) = MusicStatsDb::scan_music_folder(&dir);
    assert_eq!(tracks, 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_scan_nonexistent_directory() {
    let (tracks, albums) = MusicStatsDb::scan_music_folder(PathBuf::from("/nonexistent/path/that/does/not/exist").as_path());
    assert_eq!(tracks, 0);
    assert_eq!(albums, 0);
}
