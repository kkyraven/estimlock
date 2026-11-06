import '@fontsource/saira-semi-condensed/latin-300.css';
import '@fontsource/saira-semi-condensed/latin-400.css';
import '@fontsource/saira-semi-condensed/latin-500.css';
import '@fontsource/saira-semi-condensed/latin-600.css';
import './style.css';

import { getCurrentWindow } from '@tauri-apps/api/window';

import arrowLeft from '@phosphor-icons/core/assets/bold/arrow-left-bold.svg?raw';
import caretRight from '@phosphor-icons/core/assets/bold/caret-right-bold.svg?raw';
import gear from '@phosphor-icons/core/assets/bold/gear-six-bold.svg?raw';
import lockOpen from '@phosphor-icons/core/assets/bold/lock-simple-open-bold.svg?raw';
import lockClosed from '@phosphor-icons/core/assets/bold/lock-simple-bold.svg?raw';
import close from '@phosphor-icons/core/assets/bold/x-bold.svg?raw';

import { type View, api, startWindow } from './api';
import { toggle } from './components';
import { createDial } from './dial';
import { h, icon, setDisabled, setFlag, setHidden, setNumeric, setText } from './dom';
import { type SettingsTab, createSettings } from './settings';
import {
  canToggleRamp,
  eventButton,
  hasLevel,
  isAlert,
  levelText,
  rampLabel,
  statusWord,
  unlockText,
} from './status';
import type { EventKind, Init, Snapshot } from './types';

function iconButton(svg: string, label: string, onClick: () => void): HTMLButtonElement {
  const button = h('button', { type: 'button', class: 'bar-button', 'aria-label': label, title: label }, icon(svg));
  button.addEventListener('click', onClick);
  return button;
}

function textButton(text: string, className: string, onClick: () => void): HTMLButtonElement {
  const button = h('button', { type: 'button', class: className }, text);
  button.addEventListener('click', onClick);
  return button;
}

/** What the device slot says when the status word cannot be shown. */
function deviceProblem(snapshot: Snapshot): { text: string; fixable: boolean } | null {
  const fixable = !snapshot.locked && !snapshot.on;
  if (snapshot.device.state === 'notSelected') {
    return { text: fixable ? 'Choose device' : 'No device', fixable };
  }
  if (snapshot.device.state === 'lost') {
    return { text: fixable ? 'Device lost' : 'Reconnect device', fixable };
  }
  return null;
}

function build(init: Init): View {
  const appWindow = getCurrentWindow();
  // Names seen so far. A lost device drops off the active list but keeps its name in the title.
  const deviceNames = new Map<string, string>();
  let latest = init.snapshot;
  let shell = init.shell;

  // Title bar. The name ignores the pointer, so the bar under it can drag the window.
  const title = h('span', { class: 'title' });
  const back = iconButton(arrowLeft, 'Back', () => showMain());
  const settingsButton = iconButton(gear, 'Settings', () => showSettings());
  const hide = iconButton(close, 'Close', () => void appWindow.close());
  const bar = h('header', { class: 'bar', 'data-tauri-drag-region': '' }, back, title, settingsButton, hide);

  // Dial
  const dial = createDial();
  const number = h('div', { class: 'number' });
  const status = h('div', { class: 'status', 'aria-live': 'polite' });
  const fixText = h('span');
  const fix = h('button', { type: 'button', class: 'fix' }, fixText, icon(caretRight));
  fix.addEventListener('click', () => showSettings('output'));
  const problemText = h('div', { class: 'status problem' });
  const dialArea = h('section', { class: 'dial-area' }, dial.el, h('div', { class: 'readout' }, number, status, fix, problemText));

  // On and Lock
  const onSwitch = toggle({ label: 'On', states: ['Off', 'On'], onChange: (on) => void api.setOn(on) });
  const lockButton = h('button', { type: 'button', class: 'key' }, icon(lockClosed), 'Lock');
  lockButton.addEventListener('click', () => void api.lock());
  const unlockButton = h('button', { type: 'button', class: 'key' }, icon(lockOpen), 'Unlock');
  unlockButton.addEventListener('click', () => void api.beginUnlock());
  const countdown = h('span', { class: 'countdown', role: 'timer' });
  const cancelButton = textButton('Cancel', 'key cancel', () => void api.cancelUnlock());
  const unlocking = h('div', { class: 'unlocking' }, countdown, cancelButton);
  const controls = h('section', { class: 'controls' }, onSwitch.el, lockButton, unlockButton, unlocking);

  // Ramp and events
  const rampButton = textButton('', 'key', () => void api.toggleRamp());
  const eventButtons: Record<EventKind, HTMLButtonElement> = {
    begging: textButton('', 'key', () => void api.press('begging')),
    came: textButton('', 'key hot', () => void api.press('came')),
  };
  const keys = h('section', { class: 'keys' }, rampButton, eventButtons.begging, eventButtons.came);

  const mainView = h('main', { class: 'main' }, dialArea, controls, keys);
  const settings = createSettings(init.snapshot, init.shell, init.maxEffectHz);
  const frame = h('div', { class: 'frame', 'aria-hidden': 'true' });
  document.body.append(bar, mainView, settings.el, frame);

  let inSettings = false;
  const showSettings = (tab?: SettingsTab) => {
    inSettings = true;
    settings.show(tab);
    render(latest);
    back.focus();
  };
  const showMain = () => {
    inSettings = false;
    void refreshDevices();
    render(latest);
    settingsButton.focus();
  };
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && inSettings) showMain();
  });

  let knownDevice: string | null = null;
  let knownState = '';
  // Whether a list has come back, even an empty or failed one.
  let listed = false;
  async function refreshDevices() {
    const list = await api.listDevices().catch(() => []);
    for (const { id, name } of list) deviceNames.set(id, name);
    listed = true;
    render(latest);
  }

  function titleText(snapshot: Snapshot): string {
    if (inSettings) return 'Settings';
    const id = snapshot.persisted.config.deviceId;
    if (id === null) return 'estimlock';
    // Blank until the first list arrives, rather than a wrong name for a moment.
    return deviceNames.get(id) ?? (listed ? 'Unknown device' : '');
  }

  function render(snapshot: Snapshot) {
    latest = snapshot;
    const stopped = shell.engineStopped;
    // Lock cannot start from Settings, but a window must never sit in a view it may not use.
    if (snapshot.locked || stopped) inSettings = false;

    setFlag(document.body, 'data-locked', snapshot.locked);
    setFlag(document.body, 'data-on', snapshot.on && !stopped);

    setHidden(mainView, inSettings);
    setHidden(settings.el, !inSettings);
    setHidden(back, !inSettings);
    setHidden(settingsButton, inSettings || snapshot.locked || stopped);
    setText(title, titleText(snapshot));

    // Device names change rarely. Refresh when the device or its state does, never on a timer.
    const id = snapshot.persisted.config.deviceId;
    if (id !== knownDevice || snapshot.device.state !== knownState) {
      knownDevice = id;
      knownState = snapshot.device.state;
      void refreshDevices();
    }

    if (inSettings) {
      settings.render(snapshot);
      return;
    }

    dial.update(snapshot, hasLevel(snapshot, shell));
    setNumeric(number, levelText(snapshot, shell));
    const problem = stopped ? null : deviceProblem(snapshot);
    setHidden(status, problem !== null);
    setHidden(fix, !problem?.fixable);
    setHidden(problemText, problem === null || problem.fixable);
    if (problem) {
      setText(problem.fixable ? fixText : problemText, problem.text);
    } else {
      const word = statusWord(snapshot, shell);
      setText(status, word);
      setFlag(status, 'data-hot', isAlert(word));
    }

    onSwitch.set(snapshot.on);
    // Off is refused while locked. On is allowed, as long as there is a device to hold.
    onSwitch.setDisabled(stopped || (snapshot.on ? snapshot.locked : snapshot.device.state === 'notSelected'));

    // These replace each other, so focus has to follow or the keyboard loses its place.
    const lockKeys = [lockButton, unlockButton, cancelButton];
    const hadFocus = lockKeys.some((key) => key === document.activeElement);

    // A stopped engine counts nothing down, so its last number would be stale.
    const remaining = stopped ? null : snapshot.unlockRemaining;
    setHidden(lockButton, snapshot.locked);
    setDisabled(lockButton, stopped || snapshot.device.state === 'notSelected');
    setHidden(unlockButton, !snapshot.locked || remaining !== null);
    setDisabled(unlockButton, stopped);
    setHidden(unlocking, remaining === null);
    if (remaining !== null) setNumeric(countdown, unlockText(remaining));
    if (hadFocus && document.activeElement === document.body) {
      lockKeys.find((key) => key.offsetParent !== null)?.focus();
    }

    setText(rampButton, rampLabel(snapshot));
    setFlag(rampButton, 'data-active', snapshot.rampRunning && !stopped);
    setDisabled(rampButton, !canToggleRamp(snapshot, shell));
    for (const kind of ['begging', 'came'] as const) {
      const state = eventButton(snapshot, shell, kind);
      const button = eventButtons[kind];
      setNumeric(button, state.label);
      setDisabled(button, state.disabled);
      setFlag(button, 'data-active', state.busy);
      setFlag(button, 'data-cooling', state.cooling);
    }
  }

  return {
    render,
    // A stopped engine sends no more snapshots, so this has to redraw by itself.
    setShell(next) {
      shell = next;
      settings.setShell(next);
      render(latest);
    },
  };
}

void startWindow(build);
document.addEventListener('contextmenu', (event) => event.preventDefault());
