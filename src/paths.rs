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

pub fn models_dir() -> PathBuf {
    let p = app_support_dir().join("models");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn whisper_model_path() -> PathBuf {
    models_dir().join("ggml-large-v3-turbo.bin")
}

pub fn transcripts_dir() -> PathBuf {
    let p = app_support_dir().join("transcripts");
    let _ = std::fs::create_dir_all(&p);
    p
}
