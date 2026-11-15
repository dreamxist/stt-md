//! The log file of an app that has no terminal.
//!
//! Launched from Finder the app has nowhere to print, so every diagnostic it
//! writes would be discarded. Standard output and error are pointed at a pipe
//! and a thread copies each line to the file with the time in front: without
//! it there was no telling which meeting a "restarted tap" belonged to. Going
//! through the pipe rather than a logging macro also stamps what whisper.cpp
//! and ScreenCaptureKit print on their own.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::time::Duration;

/// Past this the log is moved to `stt-md.log.1` at startup; one old file is kept.
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// How long a dying process waits for the copier thread to drain the pipe.
const DRAIN: Duration = Duration::from_millis(120);

/// Redirects stdout and stderr to `path` when there is no terminal reading
/// them. Returns whether it did.
pub fn redirect_to_file(path: &Path) -> bool {
    if unsafe { libc::isatty(libc::STDERR_FILENO) } == 1 {
        return false;
    }
    rotate(path, MAX_LOG_BYTES);
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return false;
    };

    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        // No pipe: fall back to the raw file, without timestamps.
        unsafe {
            libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO);
            libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
        }
        return true;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    unsafe {
        libc::dup2(write_fd, libc::STDOUT_FILENO);
        libc::dup2(write_fd, libc::STDERR_FILENO);
        libc::close(write_fd);
    }

    let spawned = std::thread::Builder::new().name("log".into()).spawn(move || {
        let mut reader = BufReader::new(unsafe { File::from_raw_fd(read_fd) });
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = file.write_all(stamp(&line).as_slice());
                }
            }
        }
    });
    if spawned.is_err() {
        return false;
    }

    // A panic's message is the line that matters most, and the process may be
    // gone before the copier thread gets to it.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::thread::sleep(DRAIN);
    }));
    true
}

/// Gives the copier thread time to write what was just printed. Call before
/// `process::exit`, which doesn't wait for other threads.
pub fn drain() {
    let _ = std::io::stdout().flush();
    std::thread::sleep(DRAIN);
}

fn stamp(line: &[u8]) -> Vec<u8> {
    // Blank lines stay blank: they separate runs.
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return line.to_vec();
    }
    let mut out = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S ")
        .to_string()
        .into_bytes();
    out.extend_from_slice(line);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

fn rotate(path: &Path, max_bytes: u64) {
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > max_bytes);
    if too_big {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_prefixes_the_time_and_keeps_blank_lines() {
        let stamped = String::from_utf8(stamp(b"[stt-md] hola\n")).unwrap();
        assert!(stamped.ends_with(" [stt-md] hola\n"), "{stamped}");
        // "YYYY-MM-DD HH:MM:SS " in front.
        assert_eq!(stamped.len(), "[stt-md] hola\n".len() + 20);
        assert_eq!(stamp(b"\n"), b"\n");
        assert!(stamp(b"sin salto").ends_with(b"sin salto\n"));
    }

    #[test]
    fn rotate_moves_a_big_log_aside_and_leaves_a_small_one() {
        let dir = std::env::temp_dir().join(format!("stt-md-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stt-md.log");
        std::fs::write(&path, vec![b'x'; 100]).unwrap();
        rotate(&path, 1_000);
        assert!(path.exists());
        rotate(&path, 50);
        assert!(!path.exists());
        assert_eq!(std::fs::read(dir.join("stt-md.log.1")).unwrap().len(), 100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
