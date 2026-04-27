use std::process::Command;
use std::thread;

pub fn play_start() {
    play_async("/System/Library/Sounds/Tink.aiff");
}

pub fn play_stop() {
    play_async("/System/Library/Sounds/Pop.aiff");
}

fn play_async(path: &'static str) {
    thread::spawn(move || {
        let _ = Command::new("afplay").arg(path).status();
    });
}
