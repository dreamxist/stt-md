use std::path::PathBuf;

/// Everything the app keeps: config, recordings, queue, log.
///
/// `STT_MD_HOME` moves it elsewhere, which is how a development build or a
/// test runs next to the installed app without sharing its lock, its queue or
/// the real vault its config points at.
pub fn app_support_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("STT_MD_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    dirs::data_dir()
        .expect("HOME data_dir resolves on macOS")
        .join("stt-md")
}

fn subdir(name: &str) -> PathBuf {
    let p = app_support_dir().join(name);
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn recordings_dir() -> PathBuf {
    subdir("recordings")
}

pub fn models_dir() -> PathBuf {
    subdir("models")
}

pub fn transcripts_dir() -> PathBuf {
    subdir("transcripts")
}

/// One JSON file per recording that has not become a note yet.
pub fn queue_dir() -> PathBuf {
    subdir("queue")
}

pub fn log_file() -> PathBuf {
    app_support_dir().join("stt-md.log")
}

pub fn history_file() -> PathBuf {
    app_support_dir().join("history.jsonl")
}

pub fn socket_path() -> PathBuf {
    app_support_dir().join("stt-md.sock")
}
