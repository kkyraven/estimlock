// Written by hand to match the engine's camelCase serde output in crates/engine/src/
// (machine.rs Snapshot, config.rs Config, wave.rs Effect) and src-tauri/src/commands.rs.
// Change these together with the Rust types: nothing generates or checks them.

export type EventKind = 'begging' | 'came';
export type Stage = 'hold' | 'ramp' | 'spike' | 'recovery';
export type Wave = 'none' | 'sine' | 'saw' | 'square';

export type DeviceStatus =
  | { state: 'notSelected' }
  | { state: 'connecting' }
  | { state: 'connected' }
  | { state: 'lost'; error: string };

export interface Effect {
  wave: Wave;
  hz: number;
  depth: number;
}

export interface StageEffects {
  hold: Effect;
  ramp: Effect;
  spike: Effect;
  recovery: Effect;
}

export interface RampConfig {
  rate: number;
  target: number;
}

export interface EventConfig {
  chance: number;
  spikeMin: number;
  spikeMax: number;
  spikeMinSecs: number;
  spikeMaxSecs: number;
  delayMinSecs: number;
  delayMaxSecs: number;
  drop: number;
  recoverSecs: number;
  curveK: number;
  cooldownSecs: number;
}

export interface Config {
  deviceId: string | null;
  ceiling: number;
  ramp: RampConfig;
  begging: EventConfig;
  came: EventConfig;
  effects: StageEffects;
  panicHotkey: boolean;
}

export interface PersistedState {
  config: Config;
  locked: boolean;
  /** The set level. 0 means unset: the first On adopts the device's level. */
  level: number;
  base: number;
  on: boolean;
  rampRunning: boolean;
}

export interface Snapshot {
  output: number;
  base: number;
  stage: Stage;
  event: EventKind | null;
  pending: boolean;
  rampRunning: boolean;
  on: boolean;
  locked: boolean;
  unlockRemaining: number | null;
  cooldowns: Record<EventKind, number>;
  device: DeviceStatus;
  persisted: PersistedState;
}

export interface Endpoint {
  id: string;
  name: string;
}

export interface ShellInfo {
  widgetVisible: boolean;
  startWithWindows: boolean;
  engineStopped: boolean;
  saveFailed: boolean;
  panicFailed: boolean;
}

export interface Init {
  snapshot: Snapshot;
  shell: ShellInfo;
  maxEffectHz: number;
}
