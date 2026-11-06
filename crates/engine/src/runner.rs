//! Drives a [`Machine`] against a real device on its own thread.
//!
//! The thread owns COM, the device and the machine. Callers only touch a channel and a
//! snapshot, so a slow or hung device can never stall them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{PersistedState, TICK_HZ};
use crate::device::{DeviceError, DeviceOpener, VolumeDevice};
use crate::machine::{Command, DeviceReading, DeviceStatus, DeviceWrite, Machine, Snapshot};

/// How often to retry a missing device. Enumeration is too heavy to do every tick.
const RETRY_SECS: f64 = 1.0;

/// How long dropping a `Runner` waits for the last write before abandoning a hung device.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(1);

/// How long the engine may go without finishing a tick before it counts as stopped.
const HEARTBEAT_LIMIT: Duration = Duration::from_secs(2);

/// How long a stale heartbeat must persist before it counts. Waking from sleep makes the
/// heartbeat look hours old until the engine's next tick, and that must not read as a hang,
/// because `alive` going false allows Quit on a locked session.
const STALE_GRACE: Duration = Duration::from_millis(500);

enum Message {
    Command(Command),
    Shutdown,
}

/// When the engine last finished a tick. Atomic, so a check never waits on a stalled tick.
struct Heartbeat {
    started: Instant,
    last_beat_ms: AtomicU64,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last_beat_ms: AtomicU64::new(0),
        }
    }

    fn beat(&self) {
        let elapsed = self.started.elapsed().as_millis() as u64;
        self.last_beat_ms.store(elapsed, Ordering::Relaxed);
    }

    fn age(&self) -> Duration {
        let last_beat = Duration::from_millis(self.last_beat_ms.load(Ordering::Relaxed));
        self.started.elapsed().saturating_sub(last_beat)
    }
}

/// Handle to the engine thread. Dropping it stops the thread. That is the only stop there is
/// and it ignores the lock, so the shell must refuse to quit while `Snapshot::locked` is true,
/// unless `alive` says the snapshot is frozen.
pub struct Runner {
    messages: Sender<Message>,
    snapshot: Arc<Mutex<Snapshot>>,
    stopped: Receiver<()>,
    thread: JoinHandle<()>,
    heartbeat: Arc<Heartbeat>,
    stale_since: Mutex<Option<Instant>>,
}

impl Runner {
    /// Starts the engine. `make_opener` runs on the engine thread, so the opener can hold
    /// thread-bound resources such as COM.
    pub fn spawn<O, F>(state: PersistedState, make_opener: F) -> Self
    where
        O: DeviceOpener,
        F: FnOnce() -> O + Send + 'static,
    {
        let machine = Machine::new(state);
        let snapshot = Arc::new(Mutex::new(machine.snapshot(0.0, initial_status(&machine))));
        let (messages, inbox) = mpsc::channel();
        let (finished, stopped) = mpsc::channel();
        let shared = Arc::clone(&snapshot);
        let heartbeat = Arc::new(Heartbeat::new());
        let beats = Arc::clone(&heartbeat);

        let thread = thread::spawn(move || {
            let core = Core::new(machine, make_opener(), shared);
            run(core, inbox, &beats);
            // Ignored: nobody is waiting if the handle's drop already timed out.
            let _ = finished.send(());
        });
        Self {
            messages,
            snapshot,
            stopped,
            thread,
            heartbeat,
            stale_since: Mutex::new(None),
        }
    }

    /// Queues a command for the next tick. Never blocks. A refused command changes nothing.
    pub fn send(&self, command: Command) {
        // Only fails once the engine thread is gone, when there is nothing left to command.
        let _ = self.messages.send(Message::Command(command));
    }

    /// The state as of the latest tick. The lock is only ever held to swap in a finished
    /// snapshot, never across a device call.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether the engine is running and ticked recently. False means `snapshot` is frozen:
    /// the thread died, or a device call is hanging and may yet return.
    pub fn alive(&self) -> bool {
        if self.thread.is_finished() {
            return false;
        }
        let mut stale_since = self
            .stale_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.heartbeat.age() <= HEARTBEAT_LIMIT {
            *stale_since = None;
            return true;
        }
        stale_since.get_or_insert_with(Instant::now).elapsed() < STALE_GRACE
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // Failures here mean the thread already ended.
        let _ = self.messages.send(Message::Shutdown);
        let _ = self.stopped.recv_timeout(SHUTDOWN_WAIT);
    }
}

fn initial_status(machine: &Machine) -> DeviceStatus {
    match machine.config().device_id {
        Some(_) => DeviceStatus::Connecting,
        None => DeviceStatus::NotSelected,
    }
}

/// The tick loop. A tick that overruns skips ahead rather than bursting to catch up: time is
/// read from the clock, so the machine stays correct at any rate.
fn run<O: DeviceOpener>(mut core: Core<O>, inbox: Receiver<Message>, heartbeat: &Heartbeat) {
    let period = Duration::from_secs_f64(1.0 / f64::from(TICK_HZ));
    let started = Instant::now();
    let mut next = started;
    loop {
        let now = started.elapsed().as_secs_f64();
        loop {
            match inbox.try_recv() {
                Ok(Message::Command(command)) => core.handle(command, now),
                Err(TryRecvError::Empty) => break,
                Ok(Message::Shutdown) | Err(TryRecvError::Disconnected) => {
                    core.shutdown(now);
                    return;
                }
            }
        }
        core.step(now);
        heartbeat.beat();

        next += period;
        let current = Instant::now();
        if next > current {
            thread::sleep(next - current);
        } else {
            next = current;
        }
    }
}

/// One tick's work, split from the thread and the clock so tests can drive it directly.
struct Core<O: DeviceOpener> {
    // Declared before `opener` so the device drops first: it may depend on the opener's COM.
    device: Option<O::Device>,
    opener: O,
    machine: Machine,
    rng: fastrand::Rng,
    status: DeviceStatus,
    next_open: f64,
    snapshot: Arc<Mutex<Snapshot>>,
}

impl<O: DeviceOpener> Core<O> {
    fn new(machine: Machine, opener: O, snapshot: Arc<Mutex<Snapshot>>) -> Self {
        Self {
            device: None,
            opener,
            status: initial_status(&machine),
            machine,
            rng: fastrand::Rng::new(),
            next_open: 0.0,
            snapshot,
        }
    }

    fn handle(&mut self, command: Command, now: f64) {
        let device_id = self.machine.config().device_id.clone();
        // Ignored: a refused command changes nothing, and the snapshot shows the real state.
        let _ = self.machine.apply(command, now, &mut self.rng);
        if self.machine.config().device_id != device_id {
            self.device = None;
            self.next_open = now;
        }
    }

    fn step(&mut self, now: f64) {
        self.connect(now);
        let reading = self.read(now);
        if let Some(write) = self.machine.tick(now, reading) {
            self.write(write, now);
        }
        let snapshot = self.machine.snapshot(now, self.status.clone());
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = snapshot;
    }

    /// One last tick so an exit in the middle of a spike settles like turning Off does.
    fn shutdown(&mut self, now: f64) {
        self.machine.shutdown(now);
        self.step(now);
    }

    fn connect(&mut self, now: f64) {
        if self.device.is_some() {
            return;
        }
        let Some(id) = self.machine.config().device_id.as_deref() else {
            self.status = DeviceStatus::NotSelected;
            return;
        };
        if now < self.next_open {
            return;
        }
        self.next_open = now + RETRY_SECS;
        match self.opener.open(id) {
            Ok(device) => {
                self.device = Some(device);
                self.status = DeviceStatus::Connected;
            }
            Err(error) => self.status = DeviceStatus::Lost { error: error.0 },
        }
    }

    fn read(&mut self, now: f64) -> Option<DeviceReading> {
        let device = self.device.as_ref()?;
        let reading = read_device(device);
        self.keep_if_ok(reading, now)
    }

    fn write(&mut self, write: DeviceWrite, now: f64) {
        let Some(device) = self.device.as_mut() else {
            return;
        };
        let readback = write_device(device, write);
        if let Some(volume) = self.keep_if_ok(readback, now) {
            self.machine.note_readback(volume);
        }
    }

    /// Any device failure counts as the device being gone: drop it and retry from scratch.
    fn keep_if_ok<T>(&mut self, result: Result<T, DeviceError>, now: f64) -> Option<T> {
        result
            .map_err(|error| {
                self.device = None;
                self.next_open = now + RETRY_SECS;
                self.status = DeviceStatus::Lost { error: error.0 };
            })
            .ok()
    }
}

fn read_device(device: &impl VolumeDevice) -> Result<DeviceReading, DeviceError> {
    Ok(DeviceReading {
        volume: device.volume()?,
        muted: device.muted()?,
    })
}

/// Performs a write and returns what the device reports afterwards.
fn write_device(device: &mut impl VolumeDevice, write: DeviceWrite) -> Result<f64, DeviceError> {
    device.set_volume(write.volume)?;
    if write.unmute {
        device.set_muted(false)?;
    }
    device.volume()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::machine::EventKind;

    #[derive(Default)]
    struct FakeState {
        volume: f64,
        muted: bool,
        gone: bool,
        /// Blocks reads until cleared, like a hung driver.
        hang: bool,
        /// Panics on the next read.
        panic: bool,
        /// Rounds written levels to this step, like hardware with coarse volume steps.
        step: Option<f64>,
        opens: u32,
    }

    type Shared = Arc<Mutex<FakeState>>;

    struct FakeDevice(Shared);

    impl FakeDevice {
        fn state(&self) -> Result<std::sync::MutexGuard<'_, FakeState>, DeviceError> {
            let state = self.0.lock().unwrap();
            if state.gone {
                Err(DeviceError("gone".into()))
            } else {
                Ok(state)
            }
        }
    }

    impl VolumeDevice for FakeDevice {
        fn volume(&self) -> Result<f64, DeviceError> {
            while self.0.lock().unwrap().hang {
                thread::sleep(Duration::from_millis(5));
            }
            let panic = self.0.lock().unwrap().panic;
            assert!(!panic, "driver blew up");
            Ok(self.state()?.volume)
        }

        fn set_volume(&mut self, level: f64) -> Result<(), DeviceError> {
            let mut state = self.state()?;
            state.volume = state
                .step
                .map_or(level, |step| (level / step).round() * step);
            Ok(())
        }

        fn muted(&self) -> Result<bool, DeviceError> {
            Ok(self.state()?.muted)
        }

        fn set_muted(&mut self, muted: bool) -> Result<(), DeviceError> {
            self.state()?.muted = muted;
            Ok(())
        }
    }

    struct FakeOpener(Shared);

    impl DeviceOpener for FakeOpener {
        type Device = FakeDevice;

        fn open(&mut self, _endpoint_id: &str) -> Result<FakeDevice, DeviceError> {
            let mut state = self.0.lock().unwrap();
            state.opens += 1;
            if state.gone {
                return Err(DeviceError("not found".into()));
            }
            Ok(FakeDevice(Arc::clone(&self.0)))
        }
    }

    fn state_with_device() -> PersistedState {
        let config = Config {
            device_id: Some("fake".into()),
            ceiling: 1.0,
            ..Config::default()
        };
        PersistedState {
            config,
            ..Default::default()
        }
    }

    fn core(state: PersistedState, fake: &Shared) -> Core<FakeOpener> {
        let machine = Machine::new(state);
        let snapshot = machine.snapshot(0.0, DeviceStatus::NotSelected);
        let snapshot = Arc::new(Mutex::new(snapshot));
        Core::new(machine, FakeOpener(Arc::clone(fake)), snapshot)
    }

    fn fake(volume: f64) -> Shared {
        Arc::new(Mutex::new(FakeState {
            volume,
            ..Default::default()
        }))
    }

    fn published(core: &Core<FakeOpener>) -> Snapshot {
        core.snapshot.lock().unwrap().clone()
    }

    #[test]
    fn no_device_id_means_nothing_is_opened() {
        let fake = fake(0.4);
        let mut core = core(PersistedState::default(), &fake);
        core.step(0.0);
        assert_eq!(fake.lock().unwrap().opens, 0);
        assert_eq!(published(&core).device, DeviceStatus::NotSelected);
    }

    #[test]
    fn device_loss_is_reported_retried_and_enforcement_resumes() {
        let fake = fake(0.4);
        let mut core = core(state_with_device(), &fake);
        core.handle(Command::SetOn(true), 0.0);
        core.step(0.0);
        assert_eq!(published(&core).device, DeviceStatus::Connected);

        fake.lock().unwrap().gone = true;
        core.step(0.02);
        assert!(matches!(published(&core).device, DeviceStatus::Lost { .. }));
        let opens = fake.lock().unwrap().opens;
        core.step(0.04);
        assert_eq!(
            fake.lock().unwrap().opens,
            opens,
            "retry waits for RETRY_SECS"
        );
        core.step(1.1);
        assert_eq!(fake.lock().unwrap().opens, opens + 1);

        {
            let mut state = fake.lock().unwrap();
            state.gone = false;
            state.volume = 1.0;
            state.muted = true;
        }
        core.step(2.2);
        assert_eq!(published(&core).device, DeviceStatus::Connected);
        let state = fake.lock().unwrap();
        assert_eq!(state.volume, 0.4, "a device that reset itself is put back");
        assert!(!state.muted);
    }

    #[test]
    fn coarse_device_steps_do_not_creep_the_base() {
        let fake = fake(0.4);
        fake.lock().unwrap().step = Some(0.15);
        let mut core = core(state_with_device(), &fake);
        core.rng = fastrand::Rng::with_seed(1);
        core.handle(Command::SetOn(true), 0.0);
        core.step(0.0);

        // Long enough for the whole spike and recovery, so many levels get rounded.
        core.handle(Command::Press(EventKind::Came), 0.1);
        for tick in 0..1000 {
            core.step(0.1 + f64::from(tick) * 0.1);
        }
        let snapshot = published(&core);
        assert_eq!(snapshot.event, None, "event finished");
        assert_eq!(snapshot.base, 0.4);
    }

    #[test]
    fn a_device_rounding_past_the_ceiling_is_stepped_under_it() {
        let fake = fake(0.3);
        fake.lock().unwrap().step = Some(0.15);
        let mut state = state_with_device();
        state.config.ceiling = 0.55;
        state.level = 0.55;
        let mut core = core(state, &fake);
        core.handle(Command::SetOn(true), 0.0);
        for tick in 0..20 {
            core.step(f64::from(tick) * 0.02);
        }
        let volume = fake.lock().unwrap().volume;
        assert!(volume <= 0.55, "device at {volume}");
        assert_eq!(
            published(&core).output,
            volume,
            "shows the device, not the aim"
        );
        assert_eq!(published(&core).base, 0.55);
    }

    fn wait_for(mut condition: impl FnMut() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn running(fake: &Shared) -> Runner {
        let shared = Arc::clone(fake);
        let runner = Runner::spawn(state_with_device(), move || FakeOpener(shared));
        wait_for(
            || runner.snapshot().device == DeviceStatus::Connected,
            "connected",
        );
        runner
    }

    #[test]
    fn alive_goes_false_during_a_hang_and_returns_after_it() {
        let fake = fake(0.4);
        let runner = running(&fake);
        assert!(runner.alive());

        fake.lock().unwrap().hang = true;
        wait_for(|| !runner.alive(), "hang noticed");

        fake.lock().unwrap().hang = false;
        wait_for(|| runner.alive(), "engine recovered");
    }

    #[test]
    fn alive_goes_false_when_the_engine_panics() {
        let fake = fake(0.4);
        let runner = running(&fake);
        fake.lock().unwrap().panic = true;
        wait_for(|| !runner.alive(), "panic noticed");
    }

    #[test]
    fn thread_applies_commands_and_publishes() {
        let fake = fake(0.4);
        let shared = Arc::clone(&fake);
        let runner = Runner::spawn(state_with_device(), move || FakeOpener(shared));
        runner.send(Command::SetOn(true));

        let deadline = Instant::now() + Duration::from_secs(5);
        while runner.snapshot().base != 0.4 {
            assert!(
                Instant::now() < deadline,
                "engine never adopted the device level"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let snapshot = runner.snapshot();
        assert!(snapshot.on);
        assert_eq!(snapshot.device, DeviceStatus::Connected);
    }
}
