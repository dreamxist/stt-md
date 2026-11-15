//! Talking to the running app from outside it.
//!
//! `stt-md toggle` (or `start`, `stop`, `status`, `quit`) connects to a Unix socket
//! next to the config and the menubar instance does the rest. That is what
//! lets Raycast, Shortcuts, a Stream Deck or a shell alias start a recording
//! without reaching for the menubar.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Sender};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Toggle,
    Status,
    /// Close the app the way the menu does: a recording in progress is
    /// finished and left in the queue for the next launch.
    Quit,
}

impl Command {
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim() {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "toggle" => Some(Self::Toggle),
            "status" => Some(Self::Status),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Toggle => "toggle",
            Self::Status => "status",
            Self::Quit => "quit",
        }
    }
}

/// A command waiting for the event loop, and where to send its answer.
pub struct Request {
    pub command: Command,
    pub reply: Sender<String>,
}

/// How long a client waits for the app to answer. Starting a recording opens
/// two audio streams, which takes a moment.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Listens on `path` and hands each command to `on_request`, which runs on a
/// connection's own thread and should only pass the request along. The caller
/// holds the single-instance lock, so a socket file already there is a leftover.
pub fn serve(path: &Path, on_request: impl Fn(Request) + Send + Sync + 'static) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .with_context(|| format!("no se pudo abrir el socket {}", path.display()))?;
    let on_request = Arc::new(on_request);
    std::thread::Builder::new().name("control".into()).spawn(move || {
        // One thread per connection: a `status` must not wait behind a
        // `start` that is still opening its audio streams.
        for stream in listener.incoming().flatten() {
            let on_request = on_request.clone();
            std::thread::spawn(move || {
                if let Err(e) = answer(stream, on_request.as_ref()) {
                    eprintln!("[stt-md] control request failed: {e:#}");
                }
            });
        }
    })?;
    Ok(())
}

fn answer(stream: UnixStream, on_request: &dyn Fn(Request)) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let reply = match Command::parse(&line) {
        Some(command) => {
            let (tx, rx) = bounded(1);
            on_request(Request { command, reply: tx });
            rx.recv_timeout(REPLY_TIMEOUT)
                .unwrap_or_else(|_| "error: la app no respondió".to_string())
        }
        None => format!("error: comando desconocido «{}»", line.trim()),
    };
    writeln!(&stream, "{reply}")?;
    Ok(())
}

/// Sends `command` to the running app and returns its one-line answer.
pub fn send(path: &Path, command: Command) -> Result<String> {
    let mut stream = UnixStream::connect(path).context("stt-md no está abierto")?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT + Duration::from_secs(2)))?;
    writeln!(stream, "{}", command.word())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    Ok(reply.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn a_command_reaches_the_app_and_its_answer_comes_back() {
        // Unix socket paths are capped near 100 bytes; the temp dir is short enough.
        let path = std::env::temp_dir().join(format!("stt-md-ctl-{}.sock", std::process::id()));
        let (tx, rx) = unbounded::<Request>();
        serve(&path, move |request| {
            let _ = tx.send(request);
        })
        .unwrap();

        // Stand-in for the event loop.
        std::thread::spawn(move || {
            for request in rx {
                let _ = request.reply.send(format!("ok {:?}", request.command));
            }
        });

        assert_eq!(send(&path, Command::Toggle).unwrap(), "ok Toggle");
        assert_eq!(send(&path, Command::Status).unwrap(), "ok Status");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn without_a_running_app_the_client_says_so() {
        let path = std::env::temp_dir().join("stt-md-ctl-nobody.sock");
        let _ = std::fs::remove_file(&path);
        let err = send(&path, Command::Status).unwrap_err();
        assert!(format!("{err:#}").contains("no está abierto"));
    }

    #[test]
    fn only_known_words_are_commands() {
        assert_eq!(Command::parse("toggle\n"), Some(Command::Toggle));
        assert_eq!(Command::parse("status"), Some(Command::Status));
        assert_eq!(Command::parse("quit"), Some(Command::Quit));
        assert_eq!(Command::parse("rm -rf"), None);
        assert_eq!(Command::parse("--selftest"), None);
    }
}
