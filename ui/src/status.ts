import type { EventKind, ShellInfo, Snapshot } from './types';

export const eventNames: Record<EventKind, string> = { begging: 'Begging', came: 'I came' };

const ENGINE_STOPPED = 'Engine stopped';
const SAVE_FAILED = 'Save failed';
const PANIC_FAILED = 'Panic key failed';
const DEVICE_LOST = 'Device lost';
const PUNISHED = 'Punished';
const ALERTS = new Set([ENGINE_STOPPED, SAVE_FAILED, PANIC_FAILED, DEVICE_LOST, PUNISHED]);

/** Whether a status word should stand out. */
export function isAlert(word: string): boolean {
  return ALERTS.has(word);
}

function eventWord(snapshot: Snapshot): string | null {
  if (!snapshot.on) return null;
  if (snapshot.pending) return 'Waiting';
  if (snapshot.stage === 'spike') return PUNISHED;
  if (snapshot.stage === 'recovery') return 'Recovering';
  return null;
}

/** On with the ramp stopped and nothing left to climb: Start would stop on the next tick. */
function atTarget(snapshot: Snapshot): boolean {
  return (
    snapshot.on &&
    !snapshot.rampRunning &&
    snapshot.base >= snapshot.persisted.config.ramp.target
  );
}

export function unlockText(remaining: number): string {
  return `Unlocks in ${Math.ceil(remaining)}s`;
}

/** The one status word under the number. Device problems replace it, see `deviceProblem`. */
export function statusWord(snapshot: Snapshot, shell: ShellInfo): string {
  if (shell.engineStopped) return ENGINE_STOPPED;
  const event = eventWord(snapshot);
  if (event) return event;
  if (shell.panicFailed) return PANIC_FAILED;
  if (shell.saveFailed) return SAVE_FAILED;
  if (!snapshot.on) return 'Off';
  return snapshot.stage === 'ramp' ? 'Ramp' : 'Hold';
}

/** The widget's right slot: the most urgent thing that applies, else the ramp state. */
export function widgetStatus(snapshot: Snapshot, shell: ShellInfo): string {
  if (shell.engineStopped) return ENGINE_STOPPED;
  if (snapshot.device.state === 'lost') return DEVICE_LOST;
  if (snapshot.unlockRemaining !== null) return unlockText(snapshot.unlockRemaining);
  const event = eventWord(snapshot);
  if (event) return event;
  if (shell.panicFailed) return PANIC_FAILED;
  if (shell.saveFailed) return SAVE_FAILED;
  if (atTarget(snapshot)) return 'At target';
  return snapshot.rampRunning ? 'Ramp on' : 'Ramp off';
}

/**
 * Whether `output` is a real level. While Off it is the device's own, so it needs a device.
 * A stopped engine stops reporting, so its last number would be stale.
 */
export function hasLevel(snapshot: Snapshot, shell: ShellInfo): boolean {
  if (shell.engineStopped) return false;
  const { state } = snapshot.device;
  return state === 'connected' || (state === 'connecting' && snapshot.on);
}

export function percent(level: number): number {
  return Math.round(level * 100);
}

/** The volume as a number, or dashes when there is no level to show. */
export function levelText(snapshot: Snapshot, shell: ShellInfo): string {
  return hasLevel(snapshot, shell) ? String(percent(snapshot.output)) : '--';
}

/** Ramp and the event buttons need a running engine, On, and a device to act on. */
function canAct(snapshot: Snapshot, shell: ShellInfo): boolean {
  return !shell.engineStopped && snapshot.on && snapshot.device.state !== 'lost';
}

export interface EventButton {
  label: string;
  disabled: boolean;
  /** This event is the one running. */
  busy: boolean;
  /** The label is counting down. */
  cooling: boolean;
}

/** Label and state of Begging or I came. I came stays open during Begging: it pre-empts it. */
export function eventButton(snapshot: Snapshot, shell: ShellInfo, kind: EventKind): EventButton {
  const cooldown = snapshot.cooldowns[kind];
  const blocked =
    snapshot.event !== null && !(kind === 'came' && snapshot.event === 'begging');
  const cooling = cooldown > 0 && !shell.engineStopped;
  return {
    label: cooling ? `${eventNames[kind]} ${Math.ceil(cooldown)}s` : eventNames[kind],
    disabled: !canAct(snapshot, shell) || blocked || cooling,
    busy: snapshot.event === kind && !shell.engineStopped,
    cooling,
  };
}

export function canToggleRamp(snapshot: Snapshot, shell: ShellInfo): boolean {
  return canAct(snapshot, shell) && !atTarget(snapshot);
}

export function rampLabel(snapshot: Snapshot): string {
  if (snapshot.rampRunning) return 'Stop ramp';
  return atTarget(snapshot) ? 'At target' : 'Start ramp';
}
