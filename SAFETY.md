# Safety

estimlock is for adults. It is hobby software, not a medical device, and it has not been tested
or certified for use with electrical stimulation. You use it at your own risk.

estimlock changes the Windows volume of one audio device. It cannot see what is connected to that
device, what your stim box is doing, or how you are.

## Your stim box is the stop

While locked, unlocking estimlock takes 30 seconds. **Your stim box's dial and power switch are
the stop.** Keep them within reach, and never use a lock if you could not reach them.

An optional panic hotkey is in Settings, off by default. When enabled, Ctrl+Alt+Space sets the
volume to 0 and turns estimlock Off, even while locked. Enable it before you lock, because Settings
closes while locked. It does nothing if estimlock has frozen, so it does not replace the box.

If estimlock freezes, crashes or is killed, the volume stays at its last level. That can be the
level of a spike. Turn the box down.

## Before you connect anything

- Run a full session with nothing connected: your device, your settings, your effects.
- Use a wired output. Bluetooth and network speakers respond late, and estimlock can mistake a
  late response for someone raising the volume, which raises your level for the whole session.
- Use an output that only carries the stim signal. Do not use your default device, because
  notifications and other apps' sounds would play through your electrodes.
- Set a ceiling. It limits the Windows volume only, so also set your box's own output low.

## Equipment

- Only use a stim box designed for audio input.
- Never connect electrodes directly to a headphone, speaker or amplifier output.

## Your body

- Never place electrodes above the waist, across the chest, or on the head or neck.
- Do not use e-stim if you have a pacemaker or other implanted electronic device, a heart
  condition or epilepsy, or if you are pregnant. If you have any health condition, ask a doctor.
- Do not use it while asleep, driving, in water, or impaired by alcohol or drugs.

## Behaviour that can surprise you

- A locked session that was On resumes as soon as estimlock restarts, including at login if Start
  with Windows is on. Do not leave electrodes connected across a restart.
- Turning On jumps straight to your set level. It does not fade in.
- Spikes, drops and square waves change the level instantly.
- Effects, spikes and ramps change the volume, not your source. A louder source is a stronger
  signal at the same volume.

## Hearing

If you lock speakers or headphones instead of a stim box, spikes raise the volume suddenly and a
lock holds it there. Set a low ceiling.
