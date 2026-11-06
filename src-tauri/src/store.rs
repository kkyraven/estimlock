//! Saves engine and shell state as one JSON file.
//!
//! Writes go to a temp file and then rename over the target, so a crash leaves the old state or
//! the new one, never half of each. A ramp changes `base` continuously, so routine saves are
//! throttled. Lock and On changes skip the throttle: losing one to a crash would be a way out.
//! So does a locked `base` jumping up faster than the ramp: losing a ratchet would be a way down.
//!
//! A file that exists but cannot be used fails closed: see [`Saved::fail_closed`].

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use estimlock_engine::config::PersistedState;
use serde::{Deserialize, Serialize};

const SAVE_INTERVAL: Duration = Duration::from_secs(1);

/// Rise over ramp speed that counts as a jump: half a slider step, well above tick jitter.
const JUMP_SLACK: f64 = 0.005;

/// The widget's own state. `position` is in physical pixels, `None` until first placed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WidgetState {
    pub visible: bool,
    pub position: Option<(i32, i32)>,
}

/// Everything in the file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Saved {
    pub engine: PersistedState,
    pub widget: WidgetState,
}

impl Saved {
    /// What a file that cannot be trusted loads as. It may have held a locked session, so
    /// come up locked and Off with defaults. The normal unlock gets out of it.
    fn fail_closed() -> Self {
        let mut saved = Self::default();
        saved.engine.locked = true;
        saved
    }

    /// Only a locked restart resumes `base`, so an unlocked jump, such as dragging the level
    /// slider, can wait for the throttle.
    fn differs_urgently(&self, other: &Saved, since_save: Duration) -> bool {
        let ramp_rise = self.engine.config.ramp.rate / 60.0 * since_save.as_secs_f64();
        let jumped = self.engine.base > other.engine.base + ramp_rise + JUMP_SLACK;
        self.engine.locked != other.engine.locked
            || self.engine.on != other.engine.on
            || self.engine.ramp_running != other.engine.ramp_running
            || (self.engine.locked && jumped)
    }
}

struct Progress {
    written: Saved,
    attempted: Option<Instant>,
}

pub struct Store {
    path: PathBuf,
    progress: Mutex<Progress>,
    /// Atomic, so asking never waits behind a slow disk.
    failed: AtomicBool,
}

impl Store {
    /// Opens the store in `dir` and reads it. A missing file means a first run. Any other
    /// failure fails closed, with a copy of the file kept for inspection.
    pub fn open(dir: &Path) -> (Self, Saved) {
        if let Err(error) = fs::create_dir_all(dir) {
            eprintln!("cannot create {}: {error}", dir.display());
        }
        let path = dir.join("state.json");
        let saved = load(&path);
        let progress = Mutex::new(Progress {
            written: saved.clone(),
            attempted: None,
        });
        let store = Self {
            path,
            progress,
            failed: AtomicBool::new(false),
        };
        (store, saved)
    }

    /// Whether the latest state is not on disk because writing it failed.
    pub fn save_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Writes `saved` if it changed, at most about once a second unless the change is urgent.
    /// After a failure retries wait for the throttle, urgent or not.
    pub fn save_throttled(&self, saved: &Saved) {
        let mut progress = self.progress();
        if progress.written == *saved {
            self.failed.store(false, Ordering::Relaxed);
            return;
        }
        let since_save = progress.attempted.map(|at| at.elapsed());
        let due = since_save.is_none_or(|since| since >= SAVE_INTERVAL);
        let urgent = !self.save_failed()
            && saved.differs_urgently(&progress.written, since_save.unwrap_or_default());
        if due || urgent {
            self.write(saved, &mut progress);
        }
    }

    /// Writes `saved` now if it changed.
    pub fn flush(&self, saved: &Saved) {
        let mut progress = self.progress();
        if progress.written != *saved {
            self.write(saved, &mut progress);
        }
    }

    fn progress(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self, saved: &Saved, progress: &mut Progress) {
        progress.attempted = Some(Instant::now());
        match write_atomically(&self.path, saved) {
            Ok(()) => {
                progress.written = saved.clone();
                self.failed.store(false, Ordering::Relaxed);
            }
            // Left unrecorded, so the next tick past the throttle tries again. Shown to the
            // user through `save_failed`.
            Err(error) => {
                if !self.failed.swap(true, Ordering::Relaxed) {
                    eprintln!("cannot save {}: {error}", self.path.display());
                }
            }
        }
    }
}

fn load(path: &Path) -> Saved {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Saved::default(),
        Err(error) => {
            eprintln!("cannot read {}: {error}", path.display());
            copy_aside(path);
            return Saved::fail_closed();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        eprintln!("corrupt {}: {error}", path.display());
        copy_aside(path);
        Saved::fail_closed()
    })
}

/// Copies a bad file aside, if the file can be read at all. A copy and not a move: with
/// `state.json` gone, a crash before the next save would start unlocked.
fn copy_aside(path: &Path) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    if let Err(error) = copy_to_new_name(path, stamp) {
        eprintln!("cannot copy {} aside: {error}", path.display());
    }
}

/// `create_new`, because an earlier copy must survive a second failure in the same second.
fn copy_to_new_name(path: &Path, stamp: u64) -> io::Result<()> {
    let mut source = File::open(path)?;
    let mut attempt = 0;
    loop {
        let aside = path.with_file_name(format!("state.corrupt-{stamp}-{attempt}.json"));
        match OpenOptions::new().write(true).create_new(true).open(&aside) {
            Ok(mut copy) => return io::copy(&mut source, &mut copy).map(|_| ()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(error) => return Err(error),
        }
    }
}

fn write_atomically(path: &Path, saved: &Saved) -> io::Result<()> {
    let temp = path.with_extension("json.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(saved)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory per test, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("estimlock-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn saved(base: f64) -> Saved {
        let mut saved = Saved::default();
        saved.engine.base = base;
        saved
    }

    #[test]
    fn round_trips_and_leaves_no_temp_file() {
        let dir = TempDir::new("round-trip");
        let (store, loaded) = Store::open(&dir.0);
        assert_eq!(loaded, Saved::default());

        let mut state = saved(0.4);
        state.engine.locked = true;
        state.engine.on = true;
        state.widget.position = Some((-1200, 40));
        store.flush(&state);

        assert_eq!(Store::open(&dir.0).1, state);
        assert!(!dir.0.join("state.json.tmp").exists());
    }

    fn corrupt_copies(dir: &TempDir) -> Vec<Vec<u8>> {
        fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                let name = entry.file_name().into_string().unwrap();
                name.starts_with("state.corrupt-")
            })
            .map(|entry| fs::read(entry.path()).unwrap())
            .collect()
    }

    #[test]
    fn a_missing_file_is_a_first_run_and_stays_unlocked() {
        let dir = TempDir::new("missing");
        assert_eq!(Store::open(&dir.0).1, Saved::default());
        assert!(corrupt_copies(&dir).is_empty());
    }

    #[test]
    fn corrupt_file_fails_closed_and_is_copied_aside() {
        let dir = TempDir::new("corrupt");
        fs::write(dir.0.join("state.json"), b"{ not json").unwrap();

        let (_, loaded) = Store::open(&dir.0);
        assert_eq!(loaded, Saved::fail_closed());
        assert!(loaded.engine.locked && !loaded.engine.on);
        assert_eq!(loaded.engine.config.device_id, None);
        assert_eq!(corrupt_copies(&dir), [b"{ not json".to_vec()]);
    }

    #[test]
    fn one_bad_field_does_not_unlock_a_session() {
        let dir = TempDir::new("bad-field");
        let state = br#"{"engine":{"locked":true,"on":true,"base":"loud"}}"#;
        fs::write(dir.0.join("state.json"), state).unwrap();
        assert_eq!(Store::open(&dir.0).1, Saved::fail_closed());
    }

    #[test]
    fn an_unreadable_file_fails_closed() {
        let dir = TempDir::new("unreadable");
        fs::create_dir(dir.0.join("state.json")).unwrap();
        assert_eq!(Store::open(&dir.0).1, Saved::fail_closed());
    }

    #[test]
    fn a_later_save_leaves_the_copies_alone() {
        let dir = TempDir::new("copies");
        fs::write(dir.0.join("state.json"), b"first").unwrap();
        Store::open(&dir.0);
        fs::write(dir.0.join("state.json"), b"second").unwrap();
        let (store, _) = Store::open(&dir.0);
        store.flush(&saved(0.4));

        let mut copies = corrupt_copies(&dir);
        copies.sort();
        assert_eq!(copies, [b"first".to_vec(), b"second".to_vec()]);
        assert_eq!(Store::open(&dir.0).1, saved(0.4));
    }

    #[test]
    fn save_failure_is_reported_until_a_save_succeeds() {
        let dir = TempDir::new("save-failed");
        let (store, _) = Store::open(&dir.0);
        fs::create_dir(dir.0.join("state.json.tmp")).unwrap();

        store.save_throttled(&saved(0.1));
        assert!(store.save_failed());

        fs::remove_dir(dir.0.join("state.json.tmp")).unwrap();
        store.flush(&saved(0.1));
        assert!(!store.save_failed());
        assert_eq!(Store::open(&dir.0).1, saved(0.1));
    }

    #[test]
    fn a_failure_clears_once_nothing_is_unsaved() {
        let dir = TempDir::new("save-reverted");
        let (store, _) = Store::open(&dir.0);
        fs::create_dir(dir.0.join("state.json.tmp")).unwrap();
        store.save_throttled(&saved(0.1));
        assert!(store.save_failed());

        store.save_throttled(&Saved::default());
        assert!(!store.save_failed());
    }

    #[test]
    fn routine_changes_are_throttled_but_a_lock_is_not() {
        let dir = TempDir::new("throttle");
        let (store, _) = Store::open(&dir.0);
        let on_disk = || Store::open(&dir.0).1;

        store.save_throttled(&saved(0.1));
        assert_eq!(on_disk().engine.base, 0.1, "first change writes at once");

        store.save_throttled(&saved(0.2));
        assert_eq!(on_disk().engine.base, 0.1, "second change waits");

        let mut locked = saved(0.3);
        locked.engine.locked = true;
        store.save_throttled(&locked);
        assert!(on_disk().engine.locked, "lock skips the throttle");
    }

    fn locked(base: f64) -> Saved {
        let mut state = saved(base);
        state.engine.locked = true;
        state
    }

    #[test]
    fn a_locked_jump_skips_the_throttle_but_ramp_speed_does_not() {
        let dir = TempDir::new("jump");
        let (store, _) = Store::open(&dir.0);
        let on_disk = || Store::open(&dir.0).1.engine.base;

        store.save_throttled(&locked(0.4));
        store.save_throttled(&locked(0.4005));
        assert_eq!(on_disk(), 0.4, "a ramp-sized rise waits");

        store.save_throttled(&locked(0.6));
        assert_eq!(on_disk(), 0.6, "a ratchet does not");

        store.save_throttled(&locked(0.5));
        assert_eq!(on_disk(), 0.6, "only upward jumps are urgent");
    }

    #[test]
    fn an_unlocked_jump_waits_for_the_throttle() {
        let dir = TempDir::new("unlocked-jump");
        let (store, _) = Store::open(&dir.0);
        store.save_throttled(&saved(0.1));
        store.save_throttled(&saved(0.6));
        assert_eq!(Store::open(&dir.0).1.engine.base, 0.1);
    }
}
