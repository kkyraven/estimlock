# estimlock design

Windows tray app that holds one audio device at a set volume, only lets it rise, and runs
timed volume events. Stack: Tauri 2 (Rust engine, web UI), WASAPI `IAudioEndpointVolume`.

## Output model

Every tick the engine computes one number and writes it if it changed:

    output = clamp(base * event_gain * effect, 0, ceiling)

- `base`: the live held level. While On it only rises (ramp, or an external raise).
- `event_gain`: 1.0 at rest. >1 during a spike, <1 during recovery.
- `effect`: LFO gain in 0..=1 for the active stage. Only dips, never adds.
- `ceiling`: hard maximum. Nothing exceeds it, including external raises.

The snapshot's `output` is what the device reports. It differs from the number above on a
device that rounds.

Levels use the Windows scalar, so the number matches the Windows volume slider.
Percentages in spikes are relative: +25% on a base of 40 is 50.

## Level and base

`level` is what the user set. `base` is where the session is now. They start equal and `base`
moves away by ramping or ratcheting, so a ramp's peak never becomes the next session's start.

- Setting the level sets both, capped at the ceiling. Refused while locked.
- An unlocked Off resets `base` to `level`. So does unlocking while Off.
- An unlocked restart starts at `level`. A locked restart keeps the live `base`, so Off while
  locked is impossible and a restart loses nothing. The reset only ever happens unlocked.
- `level` 0 means unset: On adopts the device's level as `base` and `level` stays 0.
- A lower ceiling clamps `level` and the ramp target where they are used, not where they are
  stored, so dragging the ceiling down and back loses nothing. While On, `base` itself comes
  down with the ceiling and stays down until the next Off, since raising the ceiling must
  never raise the output by itself.

## On and Lock are separate

- **On**: engine enforces the floor and the buttons work. Off: engine leaves the device alone.
- **Lock**: freezes config. While locked, the app cannot be turned Off, reconfigured or quit.
- Unlock: press Unlock, a fixed 30s countdown runs, then the app is unlocked. Cancellable.
  The engine keeps running during the countdown.
- Turning On while locked is allowed. Turning Off is not.
- Lock persists across restarts. A running countdown does not.
- A locked session that was On resumes On after a restart, with its ramp state, so a restart
  is no way out. An unlocked restart comes up Off.
- The stim box's own dial and switch are the stop. An optional panic hotkey, off by default,
  backs that up: Ctrl+Alt+Space sets the device to 0 and turns Off, ignoring the lock. The lock
  stays on, and the session does not resume on restart. It is frozen with the rest of the config,
  so it cannot be enabled to escape a lock. If its keys cannot be registered, the UI says so.

## Enforcement (while On)

Each tick reads the device. If volume differs from the last value written:
- a raise: raise `base` to it (ratchet), capped at `ceiling`
- anything else, or muted: reassert (unmute, write output)

A raise must be above `base`, so nudging up during recovery below `base` is reasserted. It must
also be above every level the app commanded or read back in the last 250ms, because some
endpoints apply or report a write late or slew toward it, and catching up is not a raise.
Slower endpoints can still ratchet. During a spike `base` becomes `max(base, volume / gain)`,
so the spike's gain does not stay in `base`.

Neither a device coming back nor a gap of over 1s between ticks ratchets: after a sleep the
device may have reset itself to any level, so it is pulled back to the output.

If a write reads back above the ceiling, the device rounded it up. Later writes step down until
it reads at or under the ceiling, so the first write can overshoot for a few ticks. The step
down holds until the next On or ceiling change. It is only for our own writes: a slider
dragged past the ceiling is just pulled back, or dragging it could silence a locked session.

Going Off during a spike brings the device down to `base`. The write repeats until a reading
confirms it, stepping lower if the device rounds it back up, and is dropped if the device
changes. Off leaves the device alone otherwise.

## Ramp

Start/Stop toggles it. While running, `base` rises at `rate` %/min until `target`.
Stop pauses and `base` stays where it reached. A target above `ceiling` stops at `ceiling`.

## Events: Begging and I came

Each has its own config. A press rolls `chance`:

1. Punished: wait a random time in `delay_min_secs..delay_max_secs`, then spike by a random
   size in `spike_min..spike_max` for a random time in `spike_min_secs..spike_max_secs`.
   Then step 2. The delay defaults to 0 to 0, so the spike follows the press at once.
2. Drop by `drop` (100% = silence), then recover to `base` over `recover_secs`.

Spared presses skip step 1, so they drop straight away. Both buttons work while locked.

Recovery gain, `p = t / recover_secs`, `k` = curve steepness:

    gain = (1 - drop) + drop * ln(1 + k*p) / ln(1 + k)

Fast at first, slower later. `base` keeps moving during an event, so a running ramp continues.

Presses during an active event are ignored, so spamming cannot hold the volume down.
I came pre-empts an active Begging. Each event has an optional cooldown.

## Effects

Each stage (hold, ramp, spike, recovery) has an effect: none, sine, saw or square, with
`hz` and `depth`. `effect = 1 - depth * (1 - wave(t))`, `wave` in 0..=1.

## Device handling

Target device is stored by endpoint ID. If it disappears, the engine reports it and
retries, and resumes enforcing when it returns. Device writes never block the UI thread.

The engine thread publishes a heartbeat. `Runner::alive` is true only while the thread runs and
ticked within 2s. There is no watchdog writing to the device: a hung engine leaves it as it is.

## Shell

- Tray icon with menu; closing the main window hides it to the tray.
- Quit is refused while locked, from the moment Lock is sent. Shutting Windows down is not
  blocked. If the engine stops or hangs, the `shell` event reports `engineStopped` and Quit is
  allowed, because a frozen snapshot can never unlock. A restart resumes the lock.
- One instance only: a second launch shows the first.
- Optional widget: small, frameless, always on top, draggable, position remembered.
  Shows volume %, one status slot, and Ramp, Begging and I came buttons. The slot shows ramp
  on/off unless something more urgent applies, such as a lost device or a running unlock.
- Config and state saved as JSON in `%APPDATA%`. Debug builds can move it with
  `ESTIMLOCK_DATA_DIR`. Release builds cannot, as that would be a way past the lock.
- Load fails closed. A missing file is a first run. A file that cannot be read or parsed is
  copied aside as `state.corrupt-*.json`, and the app comes up locked and Off with default
  config, so no device. One bad field cannot unlock a session. The normal 30s unlock gets out.
- Lock, On, ramp and upward jumps of a locked `base` save at once, others about once a second.
  A failed save sets `saveFailed` in the `shell` event until a save succeeds.

## UI: Pawl, simplified

The main view shows only what you use during a session. Everything else is behind one
Settings button.

Main view, top to bottom:
- Title row: device name, Settings, close. Close hides to the tray.
- Dial: one ring of ratchet teeth, lit up to the output level, with one marker for base and one
  for the ceiling. The output number in the centre, and one status word under it. No inner arc,
  no side numbers, no target mark.
- One row: On toggle and Lock. While locked: Unlock, then `Unlocks in 18s` and Cancel.
- Three buttons: Start ramp / Stop ramp, Begging, I came.

Settings replaces the main view and has tabs: Output, Ramp, Begging, I came, Effects.
It cannot be opened while locked, so nothing ever needs a frozen state.

Widget: a small rounded bar, not the puck. Volume % and ramp state on top, three plain
buttons below. No gauge.
