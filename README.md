<img src="assets/icon.svg" width="64" height="64" alt="">

# estimlock

Holds one Windows audio device at a set volume and only lets it go up. Made for audio-driven
e-stim boxes. Adults only.

Read [SAFETY.md](SAFETY.md) before you connect anything. Your stim box's dial and power switch are
the real stop, not this app.

## What it does

- Lowering or muting the device in Windows gets undone. Raising it sets a new level. Nothing goes
  past your ceiling.
- Ramp raises the level at your rate until it reaches your target.
- Begging and I came each roll a chance of a random spike, then drop the volume and bring it back
  on a log curve, fast at first and slower later.
- Each stage can have a sine, saw or square effect, up to 10 Hz.
- Lock freezes settings and blocks Off and Quit. Unlocking takes 30 seconds, and a locked session
  comes back after a restart.
- An optional hotkey, Ctrl+Alt+Space, cuts the volume to 0 even while locked. It's off by default.
- An optional widget stays on top of everything.

## Install

Get the installer from [Releases](https://github.com/kkyraven/estimlock/releases/latest). It isn't
signed, so SmartScreen will warn you. I've only tested it on Windows 11.

It has no network code, accounts or telemetry. Settings stay in `%APPDATA%`.

## Build

    pnpm install
    pnpm build
    pnpm tauri dev

In debug builds, point `ESTIMLOCK_DATA_DIR` at a throwaway folder so test runs leave your saved
state alone. `cargo test --workspace` uses a fake device and needs no audio hardware.

Design notes are in [docs/design.md](docs/design.md), release steps in
[docs/release.md](docs/release.md).

## Licence

[MIT](LICENSE). Third-party licences are in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

Created in [T3 Code](https://t3.codes).
