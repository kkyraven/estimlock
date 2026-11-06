# Releasing

## Not yet tested

- [ ] A real audio output. Everything so far has run on a virtual device only. Measure how late
      the output reports its level, then run a full session with electrodes off.
- [ ] Panic hotkey with a real key press, locked and unlocked.
- [ ] Install, upgrade and uninstall with the installer. Check uninstall removes the Start with
      Windows entry and an upgrade keeps it.
- [ ] A `--hidden` launch does not take focus.
- [ ] Tray menu, Quit refused while locked, widget drag, transparent widget corners.
- [ ] Unplug and replug a real device during a session.

Code signing is optional. Unsigned installers get a Windows SmartScreen warning.

## Cutting a release

1. Run `pnpm notices` if dependencies changed. This needs `cargo-about`.
2. Set the same version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
3. Tag and push: `git tag v0.1.0`, then `git push origin v0.1.0`.
4. CI builds the installer and creates a draft release with a SHA-256 checksum.
5. Install the draft's installer, check it, then publish the draft.
