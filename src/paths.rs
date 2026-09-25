use std::path::PathBuf;

pub fn app_support_dir() -> PathBuf {
    dirs::data_dir()
        .expect("HOME data_dir resolves on macOS")
        .join("stt-md")
}

pub fn recordings_dir() -> PathBuf {
    let p = app_support_dir().join("recordings");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn log_file() -> PathBuf {
    app_support_dir().join("stt-md.log")
}

pub fn models_dir() -> PathBuf {
    let p = app_support_dir().join("models");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn transcripts_dir() -> PathBuf {
    let p = app_support_dir().join("transcripts");
    let _ = std::fs::create_dir_all(&p);
    p
}
