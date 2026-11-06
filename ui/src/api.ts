import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import type { Config, Endpoint, EventKind, Init, ShellInfo, Snapshot } from './types';

// One entry per Tauri command in src-tauri/src/commands.rs.
export const api = {
  getInit: () => invoke<Init>('get_init'),
  windowReady: () => invoke<void>('window_ready'),
  setOn: (on: boolean) => invoke<void>('set_on', { on }),
  lock: () => invoke<void>('lock'),
  beginUnlock: () => invoke<void>('begin_unlock'),
  cancelUnlock: () => invoke<void>('cancel_unlock'),
  toggleRamp: () => invoke<void>('toggle_ramp'),
  press: (kind: EventKind) => invoke<void>('press', { kind }),
  setLevel: (level: number) => invoke<void>('set_level', { level }),
  updateConfig: (config: Config) => invoke<void>('update_config', { config }),
  listDevices: () => invoke<Endpoint[]>('list_devices'),
  setWidgetVisible: (visible: boolean) => invoke<void>('set_widget_visible', { visible }),
  setStartWithWindows: (enabled: boolean) => invoke<void>('set_start_with_windows', { enabled }),
};

const FONT = '"Saira Semi Condensed"';
const FONT_WEIGHTS = [300, 400, 500, 600];

/** What a window hands back once built. */
export interface View {
  render(snapshot: Snapshot): void;
  setShell(shell: ShellInfo): void;
}

/**
 * Starts a window. Listens first, so nothing pushed during setup is missed, then draws the first
 * frame and only then asks for the window to be shown. The window never appears empty, and a
 * resumed locked session looks locked from its first frame.
 */
export async function startWindow(build: (init: Init) => View): Promise<void> {
  let view: View | undefined;
  let early: Snapshot | undefined;
  await listen<Snapshot>('snapshot', ({ payload }) => {
    if (view) view.render(payload);
    else early = payload;
  });
  await listen<ShellInfo>('shell', ({ payload }) => view?.setShell(payload));

  const init = await api.getInit();
  view = build(init);
  view.render(early ?? init.snapshot);
  await Promise.all(FONT_WEIGHTS.map((weight) => document.fonts.load(`${weight} 1em ${FONT}`)));
  await api.windowReady();
}
