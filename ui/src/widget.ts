import '@fontsource/saira-semi-condensed/latin-300.css';
import '@fontsource/saira-semi-condensed/latin-400.css';
import '@fontsource/saira-semi-condensed/latin-500.css';
import '@fontsource/saira-semi-condensed/latin-600.css';
import './style.css';

import { type View, api, startWindow } from './api';
import { h, setDisabled, setFlag, setNumeric } from './dom';
import { canToggleRamp, eventButton, hasLevel, isAlert, levelText, widgetStatus } from './status';
import type { EventKind, Init, Snapshot } from './types';

function key(className: string, onClick: () => void): HTMLButtonElement {
  const button = h('button', { type: 'button', class: className });
  button.addEventListener('click', onClick);
  return button;
}

function build(init: Init): View {
  let shell = init.shell;
  let latest = init.snapshot;

  const level = h('span', { class: 'widget-level' });
  const state = h('span', { class: 'widget-state' });
  const rampButton = key('key', () => void api.toggleRamp());
  rampButton.textContent = 'Ramp';
  const eventButtons: Record<EventKind, HTMLButtonElement> = {
    begging: key('key', () => void api.press('begging')),
    came: key('key hot', () => void api.press('came')),
  };
  const bar = h(
    'div',
    { class: 'widget-bar', 'data-tauri-drag-region': '' },
    h('div', { class: 'widget-top', 'data-tauri-drag-region': '' }, level, state),
    h('div', { class: 'widget-keys' }, rampButton, eventButtons.begging, eventButtons.came),
  );
  document.body.append(bar);

  const render = (snapshot: Snapshot) => {
    latest = snapshot;
    const stopped = shell.engineStopped;
    setFlag(document.body, 'data-locked', snapshot.locked);
    setFlag(document.body, 'data-on', snapshot.on && !stopped);
    const text = levelText(snapshot, shell);
    setNumeric(level, hasLevel(snapshot, shell) ? `${text}%` : text);
    const word = widgetStatus(snapshot, shell);
    setNumeric(state, word);
    setFlag(state, 'data-hot', isAlert(word));
    setFlag(rampButton, 'data-active', snapshot.rampRunning && !stopped);
    setDisabled(rampButton, !canToggleRamp(snapshot, shell));
    for (const kind of ['begging', 'came'] as const) {
      const event = eventButton(snapshot, shell, kind);
      const button = eventButtons[kind];
      setNumeric(button, event.label);
      setDisabled(button, event.disabled);
      setFlag(button, 'data-active', event.busy);
      setFlag(button, 'data-cooling', event.cooling);
    }
  };
  return {
    render,
    // A stopped engine sends no more snapshots, so this has to redraw by itself.
    setShell(next) {
      shell = next;
      render(latest);
    },
  };
}

void startWindow(build);
document.addEventListener('contextmenu', (event) => event.preventDefault());
