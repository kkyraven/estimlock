import { api } from './api';
import { type RangeSlider, type Slider, rangeSlider, segmented, slider, switchRow, toggle } from './components';
import { h, setFlag, setHidden, throttle } from './dom';
import { eventNames, percent } from './status';
import type {
  Config,
  Effect,
  Endpoint,
  EventConfig,
  EventKind,
  ShellInfo,
  Snapshot,
  StageEffects,
  Wave,
} from './types';

import prohibit from '@phosphor-icons/core/assets/bold/prohibit-bold.svg?raw';
import waveSaw from '@phosphor-icons/core/assets/bold/wave-sawtooth-bold.svg?raw';
import waveSine from '@phosphor-icons/core/assets/bold/wave-sine-bold.svg?raw';
import waveSquare from '@phosphor-icons/core/assets/bold/wave-square-bold.svg?raw';

export type SettingsTab = 'output' | 'ramp' | 'begging' | 'came' | 'effects';

const SEND_MS = 50;
// A pushed snapshot can be older than the last change. Ignore config echoes for this long.
const ECHO_MS = 250;

const SPIKE_MAX_PERCENT = 200;
// Spike time and Delay move in tenths of a second, so their sliders count tenths.
const TENTHS_PER_SEC = 10;
const SPAN_MAX_TENTHS = 15 * TENTHS_PER_SEC;
const RECOVER_MAX_SECS = 180;
const COOLDOWN_MAX_SECS = 300;
// The ramp rate moves in tenths of a percent, stored as a fraction. 0.1 to 5 %/min.
const RATE_SCALE = 1000;
const RATE_MIN_TENTHS = 1;
const RATE_MAX_TENTHS = 50;

const percentText = (value: number) => `${value}%`;
const secondsText = (value: number) => `${value}s`;
const tenthsText = (value: number) => (value / 10).toFixed(1);
const tenthsSecondsText = (value: number) => `${tenthsText(value)}s`;

function spanText(low: number, high: number, format: (value: number) => string): string {
  return low === high ? format(high) : `${format(low)} to ${format(high)}`;
}

/** Updates a control from the latest config. */
type Bind = (config: Config, snapshot: Snapshot) => void;

export interface Settings {
  el: HTMLElement;
  /** Opens on `tab`, or on the tab last used this session. */
  show(tab?: SettingsTab): void;
  render(snapshot: Snapshot): void;
  setShell(shell: ShellInfo): void;
}

/** The Settings view. Every change applies at once, there is no Save. */
const PANIC_KEYS = 'Panic: Ctrl+Alt+Space';

export function createSettings(initial: Snapshot, shell: ShellInfo, maxEffectHz: number): Settings {
  let config: Config = structuredClone(initial.persisted.config);
  let changedAt = 0;
  // Null until the first list comes back, so the empty state does not flash.
  let devices: Endpoint[] | null = null;
  let latest = initial;
  let shellInfo = shell;
  let currentTab: SettingsTab = 'output';
  const binds: Bind[] = [];

  const sendConfig = throttle(() => void api.updateConfig(config), SEND_MS);
  const sendLevel = throttle((level: number) => void api.setLevel(level), SEND_MS);
  const change = (mutate: (draft: Config) => void) => {
    mutate(config);
    changedAt = performance.now();
    sendConfig();
  };

  function percentSlider(
    label: string,
    read: (draft: Config) => number,
    write: (draft: Config, value: number) => void,
    limit?: () => number,
  ): Slider {
    const control = slider({
      label,
      min: 0,
      max: 100,
      limit,
      format: percentText,
      onInput: (value) => change((draft) => write(draft, value / 100)),
    });
    binds.push((draft) => control.set(percent(read(draft))));
    return control;
  }

  function secondsSlider(
    label: string,
    max: number,
    read: (event: EventConfig) => number,
    write: (event: EventConfig, value: number) => void,
    kind: EventKind,
  ): Slider {
    const control = slider({
      label,
      min: 0,
      max,
      format: secondsText,
      onInput: (value) => change((draft) => write(draft[kind], value)),
    });
    binds.push((draft) => control.set(read(draft[kind])));
    return control;
  }

  function spanSlider(
    label: string,
    max: number,
    format: (value: number) => string,
    scale: number,
    keys: [keyof EventConfig, keyof EventConfig],
    kind: EventKind,
  ): RangeSlider {
    const [lowKey, highKey] = keys;
    const control = rangeSlider({
      label,
      min: 0,
      max,
      format: (low, high) => spanText(low, high, format),
      onInput: (low, high) =>
        change((draft) => {
          draft[kind][lowKey] = low / scale;
          draft[kind][highKey] = high / scale;
        }),
    });
    binds.push((draft) =>
      control.set(
        Math.round(draft[kind][lowKey] * scale),
        Math.round(draft[kind][highKey] * scale),
      ),
    );
    return control;
  }

  const ceilingPercent = () => percent(config.ceiling);

  const select = h('select', { 'aria-label': 'Device' });
  select.addEventListener('change', () => change((draft) => (draft.deviceId = select.value)));
  const retry = h('button', { type: 'button', class: 'key' }, 'Retry');
  retry.addEventListener('click', () => void loadDevices());
  const noDevices = h('div', { class: 'no-devices', hidden: '' }, h('span', {}, 'No devices'), retry);

  // Keeps the current id in the list when it is not active, so the picker tells the truth.
  const fillDevices = () => {
    if (devices === null) return;
    const empty = devices.length === 0;
    const retryHadFocus = document.activeElement === retry;
    setHidden(select, empty);
    setHidden(noDevices, !empty);
    if (empty) return;
    if (retryHadFocus) select.focus();
    const id = config.deviceId;
    const options = devices.map((device) => h('option', { value: device.id }, device.name));
    if (id === null) {
      options.unshift(h('option', { value: '', disabled: '' }, 'Choose device'));
    } else if (!devices.some((device) => device.id === id)) {
      options.unshift(h('option', { value: id, disabled: '' }, 'Device lost'));
    }
    select.replaceChildren(...options);
    select.value = id ?? '';
  };

  // A failed list reads as an empty one: either way there is nothing to pick.
  const loadDevices = async () => {
    devices = await api.listDevices().catch(() => []);
    fillDevices();
  };

  function outputPanel(): HTMLElement {
    const hint = h('span', { class: 'hint' }, 'Turn Off to change');
    binds.push((draft, snapshot) => {
      if (select.value !== (draft.deviceId ?? '')) fillDevices();
      select.disabled = snapshot.on;
      setHidden(hint, !snapshot.on);
    });

    const level = slider({
      label: 'Level',
      min: 0,
      max: 100,
      limit: ceilingPercent,
      format: percentText,
      // 0 means unset: the first On adopts the device's level, so that is the one to show.
      hint: (value) => {
        if (value !== 0) return undefined;
        const known = latest.device.state === 'connected' && !latest.on;
        return known ? `Device ${percent(latest.output)}%` : 'Not set';
      },
      onInput: (value) => sendLevel(value / 100),
    });
    binds.push((_, snapshot) => level.set(percent(snapshot.persisted.level)));

    const ceiling = percentSlider(
      'Ceiling',
      (draft) => draft.ceiling,
      (draft, value) => (draft.ceiling = value),
    );

    return h(
      'div',
      { class: 'panel' },
      h('div', { class: 'field' }, h('div', { class: 'field-head' }, h('span', { class: 'label' }, 'Device'), hint), select, noDevices),
      level.el,
      ceiling.el,
      switchRow('Widget', widgetSwitch),
      switchRow('Start with Windows', startSwitch),
      h('div', { class: 'row switch-row' }, panicLabel, panicSwitch.el),
    );
  }

  function rampPanel(): HTMLElement {
    const rate = slider({
      label: 'Rate',
      min: RATE_MIN_TENTHS,
      max: RATE_MAX_TENTHS,
      format: (value) => `${tenthsText(value)}%/min`,
      onInput: (value) => change((draft) => (draft.ramp.rate = value / RATE_SCALE)),
    });
    binds.push((draft) => rate.set(Math.round(draft.ramp.rate * RATE_SCALE)));
    return h(
      'div',
      { class: 'panel' },
      rate.el,
      percentSlider(
        'Target',
        (draft) => draft.ramp.target,
        (draft, value) => (draft.ramp.target = value),
        ceilingPercent,
      ).el,
    );
  }

  function eventPanel(kind: EventKind): HTMLElement {
    const spikeSize = spanSlider('Spike size', SPIKE_MAX_PERCENT, percentText, 100, ['spikeMin', 'spikeMax'], kind);
    const limited = h('span', { class: 'row-note', hidden: '' }, 'Ceiling limits spike');
    spikeSize.el.append(limited);
    binds.push((draft, snapshot) =>
      setHidden(limited, snapshot.base * (1 + draft[kind].spikeMin) <= draft.ceiling),
    );
    return h(
      'div',
      { class: 'panel' },
      percentSlider(
        'Chance',
        (draft) => draft[kind].chance,
        (draft, value) => (draft[kind].chance = value),
      ).el,
      spanSlider('Delay', SPAN_MAX_TENTHS, tenthsSecondsText, TENTHS_PER_SEC, ['delayMinSecs', 'delayMaxSecs'], kind).el,
      spikeSize.el,
      spanSlider('Spike time', SPAN_MAX_TENTHS, tenthsSecondsText, TENTHS_PER_SEC, ['spikeMinSecs', 'spikeMaxSecs'], kind).el,
      percentSlider(
        'Drop',
        (draft) => draft[kind].drop,
        (draft, value) => (draft[kind].drop = value),
      ).el,
      secondsSlider(
        'Recovery',
        RECOVER_MAX_SECS,
        (event) => event.recoverSecs,
        (event, value) => (event.recoverSecs = value),
        kind,
      ).el,
      secondsSlider(
        'Cooldown',
        COOLDOWN_MAX_SECS,
        (event) => event.cooldownSecs,
        (event, value) => (event.cooldownSecs = value),
        kind,
      ).el,
    );
  }

  function effectBlock(stage: keyof StageEffects, title: string): HTMLElement {
    const read = (draft: Config): Effect => draft.effects[stage];
    const wave = segmented<Wave>({
      label: `${title} effect`,
      role: 'radio',
      className: 'segmented',
      choices: [
        { value: 'none', label: 'None', icon: prohibit },
        { value: 'sine', label: 'Sine', icon: waveSine },
        { value: 'saw', label: 'Saw', icon: waveSaw },
        { value: 'square', label: 'Square', icon: waveSquare },
      ],
      onChange: (value) =>
        change((draft) => {
          const effect = read(draft);
          effect.wave = value;
          // A wave with no rate or depth would do nothing, so start from something audible.
          if (value !== 'none' && effect.hz === 0) effect.hz = 1;
          if (value !== 'none' && effect.depth === 0) effect.depth = 0.25;
          bindAll();
        }),
    });
    const hz = slider({
      label: 'Hz',
      min: 0.1,
      max: maxEffectHz,
      step: 0.1,
      format: (value) => value.toFixed(1),
      onInput: (value) => change((draft) => (read(draft).hz = value)),
    });
    const depth = slider({
      label: 'Depth',
      min: 0,
      max: 100,
      format: percentText,
      onInput: (value) => change((draft) => (read(draft).depth = value / 100)),
    });
    const sliders = h('div', { class: 'effect-sliders' }, hz.el, depth.el);
    binds.push((draft) => {
      const effect = read(draft);
      wave.set(effect.wave);
      hz.set(Math.max(effect.hz, 0.1));
      depth.set(percent(effect.depth));
      setFlag(sliders, 'data-none', effect.wave === 'none');
    });
    return h(
      'div',
      { class: 'effect' },
      h('div', { class: 'effect-head' }, h('span', { class: 'label' }, title), wave.el),
      sliders,
    );
  }

  function effectsPanel(): HTMLElement {
    return h(
      'div',
      { class: 'panel effects' },
      effectBlock('hold', 'Hold'),
      effectBlock('ramp', 'Ramp'),
      effectBlock('spike', 'Spike'),
      effectBlock('recovery', 'Recovery'),
    );
  }

  const widgetSwitch = toggle({
    label: 'Widget',
    onChange: (visible) => void api.setWidgetVisible(visible),
  });
  const startSwitch = toggle({
    label: 'Start with Windows',
    onChange: (enabled) => {
      startSwitch.set(enabled);
      api.setStartWithWindows(enabled).catch(() => startSwitch.set(shellInfo.startWithWindows));
    },
  });
  const panicSwitch = toggle({
    label: 'Panic hotkey',
    onChange: (enabled) => {
      panicSwitch.set(enabled);
      change((draft) => (draft.panicHotkey = enabled));
    },
  });
  const panicLabel = h('span', { class: 'label' }, PANIC_KEYS);
  binds.push((draft) => panicSwitch.set(draft.panicHotkey));

  const setShell = (next: ShellInfo) => {
    shellInfo = next;
    widgetSwitch.set(next.widgetVisible);
    startSwitch.set(next.startWithWindows);
    panicLabel.textContent = next.panicFailed ? 'Keys in use' : PANIC_KEYS;
  };

  const panels: Record<SettingsTab, HTMLElement> = {
    output: outputPanel(),
    ramp: rampPanel(),
    begging: eventPanel('begging'),
    came: eventPanel('came'),
    effects: effectsPanel(),
  };

  const tabChoices: { value: SettingsTab; label: string }[] = [
    { value: 'output', label: 'Output' },
    { value: 'ramp', label: 'Ramp' },
    { value: 'begging', label: eventNames.begging },
    { value: 'came', label: eventNames.came },
    { value: 'effects', label: 'Effects' },
  ];
  for (const { value, label } of tabChoices) {
    panels[value].setAttribute('role', 'tabpanel');
    panels[value].setAttribute('aria-label', label);
  }
  const tabs = segmented<SettingsTab>({
    label: 'Settings',
    role: 'tab',
    className: 'tabs',
    choices: tabChoices,
    onChange: (tab) => showTab(tab),
  });
  const showTab = (tab: SettingsTab) => {
    currentTab = tab;
    for (const [name, panel] of Object.entries(panels)) setHidden(panel, name !== tab);
  };

  const bindAll = () => {
    for (const bind of binds) bind(config, latest);
  };

  setShell(shell);
  bindAll();
  return {
    el: h('section', { class: 'settings' }, tabs.el, ...Object.values(panels)),
    show(tab = currentTab) {
      tabs.set(tab);
      showTab(tab);
      void loadDevices();
    },
    render(snapshot) {
      latest = snapshot;
      if (performance.now() - changedAt > ECHO_MS) {
        config = structuredClone(snapshot.persisted.config);
      }
      bindAll();
    },
    setShell,
  };
}
