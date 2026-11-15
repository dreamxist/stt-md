//! Recordings on their way to becoming notes.
//!
//! Every recording is a small JSON file in `queue/` from the moment it starts
//! until its note is written. That file is what makes three things possible:
//! recording the next meeting while the previous one is still being
//! transcribed, picking up after a crash or a power cut instead of leaving two
//! WAVs nobody knows to reprocess, and retrying a failed meeting from the menu.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local};
use crossbeam_channel::{unbounded, Sender};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::history::{self, HistoryEntry};
use crate::pipeline::{self, NothingToTranscribe, ProcessedMeeting, RecordingInput, Stage};

/// The config the worker reads for each job. The menubar swaps it when the
/// file changes, so an edit applies to the next meeting without a relaunch.
pub type SharedConfig = Arc<RwLock<Arc<Config>>>;

/// A job that took the process down this many times is not retried on launch:
/// it would do it again, every time the app opens.
const MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Audio is still being written.
    Recording,
    /// Stopped; waiting for the worker or being processed by it.
    Pending,
    /// Processing failed; kept so it can be retried.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    /// The recording's timestamp, e.g. `2026-11-10-170018`.
    pub id: String,
    pub started_at: DateTime<Local>,
    /// Unknown until the recording is stopped.
    #[serde(default)]
    pub duration_min: Option<i64>,
    pub mic_path: PathBuf,
    #[serde(default)]
    pub sys_path: Option<PathBuf>,
    pub status: JobStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub calendar_title: Option<String>,
}

impl Job {
    pub fn for_recording(
        started_at: DateTime<Local>,
        mic_path: PathBuf,
        sys_path: Option<PathBuf>,
        calendar_title: Option<String>,
    ) -> Self {
        Self {
            id: started_at.format("%Y-%m-%d-%H%M%S").to_string(),
            started_at,
            duration_min: None,
            mic_path,
            sys_path,
            status: JobStatus::Recording,
            attempts: 0,
            last_error: None,
            calendar_title,
        }
    }

    /// "10 nov 17:00", for menus and notifications.
    pub fn label(&self) -> String {
        format!(
            "{} {} {}",
            self.started_at.day(),
            crate::usage::month_short_es(self.started_at.date_naive()),
            self.started_at.format("%H:%M")
        )
    }

    fn file(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.json", self.id))
    }

    /// Writes the job next to its siblings. Through a temp file and a rename,
    /// so a crash mid-write leaves the previous version, not half a JSON.
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{}.json.tmp", self.id));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("no se pudo escribir {}", tmp.display()))?;
        std::fs::rename(&tmp, self.file(dir))?;
        Ok(())
    }

    pub fn remove(&self, dir: &Path) {
        let _ = std::fs::remove_file(self.file(dir));
    }

    fn input(&self) -> RecordingInput {
        RecordingInput {
            mic_path: self.mic_path.clone(),
            sys_path: self.sys_path.clone(),
            started_at: self.started_at,
            duration_min: self.duration_min,
            calendar_title: self.calendar_title.clone(),
        }
    }
}

/// Every job on disk, oldest first. Files that don't parse are left alone.
pub fn load_all(dir: &Path) -> Vec<Job> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut jobs: Vec<Job> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let job = std::fs::read(&p)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Job>(&bytes).ok());
            if job.is_none() {
                eprintln!("[stt-md] ignoring unreadable job file {}", p.display());
            }
            job
        })
        .collect();
    jobs.sort_by_key(|j| j.started_at);
    jobs
}

/// What the queue holds when the app starts.
#[derive(Debug, Default, PartialEq)]
pub struct Recovered {
    /// Interrupted before their note was written: process them now.
    pub run: Vec<Job>,
    /// Failed earlier, or kept crashing: offered for a manual retry.
    pub failed: Vec<Job>,
}

/// Sorts out what was left in the queue by a previous run and updates the
/// files to match.
pub fn recover(dir: &Path) -> Recovered {
    let mut out = Recovered::default();
    for mut job in load_all(dir) {
        if !job.mic_path.exists() {
            println!("[stt-md] job {}: its audio is gone, dropping it", job.id);
            job.remove(dir);
            continue;
        }
        match job.status {
            JobStatus::Failed => out.failed.push(job),
            // Still "recording" at launch means the app died mid-meeting. The
            // WAV is valid up to its last flush; nobody measured the duration.
            JobStatus::Recording => {
                println!("[stt-md] job {}: recording was interrupted, recovering it", job.id);
                job.status = JobStatus::Pending;
                job.duration_min = None;
                let _ = job.save(dir);
                out.run.push(job);
            }
            JobStatus::Pending if job.attempts >= MAX_ATTEMPTS => {
                job.status = JobStatus::Failed;
                job.last_error = Some(format!(
                    "el procesamiento se interrumpió {} veces sin terminar",
                    job.attempts
                ));
                let _ = job.save(dir);
                out.failed.push(job);
            }
            JobStatus::Pending => out.run.push(job),
        }
    }
    out
}

/// What the worker reports back to whoever is showing the queue.
#[derive(Debug)]
pub enum JobEvent {
    Started { job: Job },
    Stage { id: String, stage: Stage },
    Done { job: Job, meeting: ProcessedMeeting },
    /// Nothing in the recording to make a note of.
    Skipped { job: Job, reason: String },
    Failed { job: Job, error: String },
}

/// Starts the thread that turns jobs into notes, one at a time, and returns
/// the channel to hand it work. `emit` is called from that thread with each
/// thing worth showing.
pub fn spawn_worker(
    dir: PathBuf,
    config: SharedConfig,
    emit: impl Fn(JobEvent) + Send + 'static,
) -> Sender<Job> {
    let (tx, rx) = unbounded::<Job>();
    let spawned = std::thread::Builder::new().name("jobs".into()).spawn(move || {
        for job in rx {
            let cfg = config.read().clone();
            run_job(job, &dir, &cfg, &emit);
            if let Some(days) = cfg.keep_recordings_days {
                sweep_old_recordings(&crate::paths::recordings_dir(), &dir, days, SystemTime::now());
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("[stt-md] could not start the job worker: {e}");
    }
    tx
}

fn run_job(mut job: Job, dir: &Path, cfg: &Config, emit: &dyn Fn(JobEvent)) {
    job.attempts += 1;
    job.status = JobStatus::Pending;
    job.last_error = None;
    if let Err(e) = job.save(dir) {
        eprintln!("[stt-md] could not save job {}: {e:#}", job.id);
    }
    println!("[stt-md] job {}: processing (attempt {})", job.id, job.attempts);
    emit(JobEvent::Started { job: job.clone() });

    let id = job.id.clone();
    let input = job.input();
    // A panic inside whisper or a writer must cost one meeting's note, not
    // the worker: every later meeting would queue up behind a dead thread.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pipeline::process(&input, cfg, &mut |stage| {
            emit(JobEvent::Stage { id: id.clone(), stage });
        })
    }))
    .unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("panic");
        Err(anyhow::anyhow!("el procesamiento se cayó: {what}"))
    });

    match result {
        Ok(meeting) => {
            job.remove(dir);
            history::append(&HistoryEntry {
                at: job.started_at,
                title: meeting.title.clone(),
                note_path: meeting.note_path.clone(),
                duration_min: meeting.duration_min,
                model: meeting.model.clone(),
            });
            println!("[stt-md] job {}: note at {}", job.id, meeting.note_path.display());
            emit(JobEvent::Done { job, meeting });
        }
        Err(e) => match e.downcast_ref::<NothingToTranscribe>() {
            Some(nothing) => {
                println!("[stt-md] job {}: skipped — {nothing}", job.id);
                job.remove(dir);
                let reason = nothing.to_string();
                emit(JobEvent::Skipped { job, reason });
            }
            None => {
                eprintln!("[stt-md] job {}: failed — {e:?}", job.id);
                let error = format!("{e:#}");
                job.status = JobStatus::Failed;
                job.last_error = Some(error.clone());
                if let Err(save) = job.save(dir) {
                    eprintln!("[stt-md] could not save job {}: {save:#}", job.id);
                }
                emit(JobEvent::Failed { job, error });
            }
        },
    }
}

/// Deletes WAVs older than `keep_days`, except those a queued job still
/// needs. Returns how many files went and how many bytes they held.
pub fn sweep_old_recordings(
    recordings: &Path,
    queue: &Path,
    keep_days: u32,
    now: SystemTime,
) -> (usize, u64) {
    let needed: HashSet<PathBuf> = load_all(queue)
        .into_iter()
        .flat_map(|j| std::iter::once(j.mic_path).chain(j.sys_path))
        .collect();
    let max_age = Duration::from_secs(keep_days as u64 * 24 * 3600);
    let Ok(entries) = std::fs::read_dir(recordings) else {
        return (0, 0);
    };
    let (mut files, mut bytes) = (0, 0);
    for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
        if path.extension().is_none_or(|x| x != "wav") || needed.contains(&path) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > max_age);
        if old && std::fs::remove_file(&path).is_ok() {
            files += 1;
            bytes += meta.len();
        }
    }
    if files > 0 {
        println!(
            "[stt-md] removed {files} recordings older than {keep_days} days ({} MB)",
            bytes / 1_000_000
        );
    }
    (files, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stt-md-jobs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn job_at(dir: &Path, hour: u32, status: JobStatus, attempts: u32) -> Job {
        let started = Local.with_ymd_and_hms(2026, 11, 10, hour, 0, 18).unwrap();
        let mic = dir.join(format!("{hour}-mic.wav"));
        std::fs::write(&mic, b"wav").unwrap();
        let mut job = Job::for_recording(started, mic, None, None);
        job.status = status;
        job.attempts = attempts;
        job.duration_min = Some(30);
        job
    }

    fn collect_events(job: Job, dir: &Path) -> Vec<JobEvent> {
        let events = std::cell::RefCell::new(Vec::new());
        run_job(job, dir, &Config::default(), &|e| events.borrow_mut().push(e));
        events.into_inner()
    }

    #[test]
    fn a_job_survives_the_trip_to_disk_and_reads_back_in_order() {
        let dir = scratch("roundtrip");
        let late = job_at(&dir, 17, JobStatus::Pending, 0);
        let early = job_at(&dir, 9, JobStatus::Failed, 1);
        late.save(&dir).unwrap();
        early.save(&dir).unwrap();
        std::fs::write(dir.join("garbage.json"), b"{not json").unwrap();

        assert_eq!(late.id, "2026-11-10-170018");
        assert_eq!(late.label(), "10 nov 17:00");
        assert_eq!(load_all(&dir), vec![early.clone(), late.clone()]);

        late.remove(&dir);
        assert_eq!(load_all(&dir), vec![early]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The meeting that was being recorded when the laptop died is the one
    /// worth the most: it must come back on the next launch by itself.
    #[test]
    fn recover_resumes_interrupted_work_and_parks_what_keeps_failing() {
        let dir = scratch("recover");
        let crashed = job_at(&dir, 9, JobStatus::Recording, 0);
        let waiting = job_at(&dir, 10, JobStatus::Pending, 1);
        let poison = job_at(&dir, 11, JobStatus::Pending, MAX_ATTEMPTS);
        let failed = job_at(&dir, 12, JobStatus::Failed, 1);
        let orphan = job_at(&dir, 13, JobStatus::Pending, 0);
        for j in [&crashed, &waiting, &poison, &failed, &orphan] {
            j.save(&dir).unwrap();
        }
        std::fs::remove_file(&orphan.mic_path).unwrap();

        let r = recover(&dir);
        let ids = |jobs: &[Job]| jobs.iter().map(|j| j.started_at.format("%H").to_string()).collect::<Vec<_>>();
        assert_eq!(ids(&r.run), vec!["09", "10"]);
        assert_eq!(ids(&r.failed), vec!["11", "12"]);

        // The interrupted recording no longer claims a duration it never had.
        assert_eq!(r.run[0].status, JobStatus::Pending);
        assert_eq!(r.run[0].duration_min, None);
        assert!(r.failed[0].last_error.as_deref().unwrap().contains("3 veces"));

        // What recover decided is what the next launch will read.
        let on_disk = load_all(&dir);
        assert_eq!(on_disk.len(), 4, "the job without audio is gone");
        assert!(on_disk.iter().all(|j| j.status != JobStatus::Recording));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_removes_only_old_wavs_no_job_is_waiting_for() {
        let dir = scratch("sweep");
        let (recordings, queue) = (dir.join("recordings"), dir.join("queue"));
        std::fs::create_dir_all(&recordings).unwrap();
        std::fs::create_dir_all(&queue).unwrap();
        for name in ["old-mic.wav", "old-queued-mic.wav", "new-mic.wav", "notes.txt"] {
            std::fs::write(recordings.join(name), vec![0u8; 10]).unwrap();
        }
        let mut queued = job_at(&dir, 9, JobStatus::Failed, 1);
        queued.mic_path = recordings.join("old-queued-mic.wav");
        queued.save(&queue).unwrap();

        // "Now" is 40 days ahead for everything; then one file is made fresh.
        let now = SystemTime::now() + Duration::from_secs(40 * 24 * 3600);
        std::fs::File::options()
            .write(true)
            .open(recordings.join("new-mic.wav"))
            .unwrap()
            .set_modified(now - Duration::from_secs(3600))
            .unwrap();

        assert_eq!(sweep_old_recordings(&recordings, &queue, 30, now), (1, 10));
        let mut left: Vec<String> = std::fs::read_dir(&recordings)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["new-mic.wav", "notes.txt", "old-queued-mic.wav"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End to end through the worker without whisper: a recording too short
    /// to hold anything is skipped and leaves no job behind.
    #[test]
    fn the_worker_skips_an_empty_recording_and_clears_its_job() {
        let dir = scratch("worker");
        let mic = dir.join("empty-mic.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        hound::WavWriter::create(&mic, spec).unwrap().finalize().unwrap();
        let mut job = Job::for_recording(Local::now(), mic, None, None);
        job.status = JobStatus::Pending;
        job.save(&dir).unwrap();

        let events = collect_events(job, &dir);
        assert!(matches!(events.first(), Some(JobEvent::Started { .. })));
        assert!(
            matches!(events.last(), Some(JobEvent::Skipped { reason, .. }) if reason.contains("menos de 1 segundo")),
            "{events:?}"
        );
        assert!(load_all(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recording_that_cannot_be_read_fails_and_stays_for_a_retry() {
        let dir = scratch("failed");
        let job = job_at(&dir, 9, JobStatus::Pending, 0); // "wav" bytes: not a WAV
        job.save(&dir).unwrap();

        assert!(matches!(collect_events(job, &dir).last(), Some(JobEvent::Failed { .. })));
        let on_disk = load_all(&dir);
        assert_eq!(on_disk.len(), 1);
        assert_eq!(on_disk[0].status, JobStatus::Failed);
        assert_eq!(on_disk[0].attempts, 1);
        assert!(on_disk[0].last_error.as_deref().unwrap().contains("no se pudo leer"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
