import { h, icon, setFlag, setNumeric } from './dom';

// Native range inputs under the styling, so keyboard, focus and touch come for free.

interface Range {
  min: number;
  max: number;
  step?: number;
}

/** Tracks whether the pointer is on an input, so pushed values do not fight a drag. */
function trackDrag(input: HTMLInputElement): () => boolean {
  let dragging = false;
  input.addEventListener('pointerdown', () => (dragging = true));
  window.addEventListener('pointerup', () => (dragging = false));
  window.addEventListener('pointercancel', () => (dragging = false));
  return () => dragging;
}

function rangeInput(range: Range, label: string): HTMLInputElement {
  return h('input', {
    type: 'range',
    min: String(range.min),
    max: String(range.max),
    step: String(range.step ?? 1),
    'aria-label': label,
  });
}

function fraction(value: number, range: Range): string {
  return String((value - range.min) / (range.max - range.min));
}

function row(label: string, track: HTMLElement, value: HTMLElement): HTMLElement {
  return h('div', { class: 'row' }, h('span', { class: 'label' }, label), track, value);
}

export interface Slider {
  el: HTMLElement;
  set(value: number): void;
}

export function slider(
  options: Range & {
    label: string;
    /** Highest value allowed right now, for levels capped by the ceiling. The run past it is shaded. */
    limit?: () => number;
    format(value: number): string;
    /** Muted text to show in place of `format`, when the value is not a real setting. */
    hint?(value: number): string | undefined;
    onInput(value: number): void;
  },
): Slider {
  const input = rangeInput(options, options.label);
  const output = h('output');
  const past = options.limit ? [h('i', { class: 'past' })] : [];
  const track = h('div', { class: 'track' }, ...past, h('i', { class: 'fill' }), input);
  const dragging = trackDrag(input);

  const show = (value: number) => {
    track.style.setProperty('--hi', fraction(value, options));
    if (options.limit) track.style.setProperty('--limit', fraction(options.limit(), options));
    const hint = options.hint?.(value);
    setFlag(output, 'data-hint', hint !== undefined);
    setNumeric(output, hint ?? options.format(value));
  };
  input.addEventListener('input', () => {
    const value = Math.min(input.valueAsNumber, options.limit?.() ?? options.max);
    input.value = String(value);
    options.onInput(value);
    show(value);
  });
  return {
    el: row(options.label, track, output),
    set(value) {
      if (dragging()) return;
      if (input.valueAsNumber !== value) input.value = String(value);
      show(input.valueAsNumber);
    },
  };
}

export interface RangeSlider {
  el: HTMLElement;
  set(low: number, high: number): void;
}

// Must match the thumb width in style.css.
const THUMB_PX = 8;
// A press this close to a thumb's centre grabs it where it is, rather than moving it.
const GRAB_PX = 10;
// How far a press on overlapping thumbs must travel before its direction counts.
const DIRECTION_PX = 2;

/** Converts between values and client x. Thumb centres stop half a thumb short of each end. */
function trackScale(track: HTMLElement, range: Range) {
  const { left, width } = track.getBoundingClientRect();
  const start = left + THUMB_PX / 2;
  const span = width - THUMB_PX;
  const size = range.max - range.min;
  return {
    toX: (value: number) => start + ((value - range.min) / size) * span,
    toValue: (x: number) => range.min + ((x - start) / span) * size,
  };
}

interface Drag {
  scale: ReturnType<typeof trackScale>;
  startX: number;
  /** Null until the direction picks one. */
  thumb: HTMLInputElement | null;
  /** Keeps a grabbed thumb from jumping to the pointer. */
  offset: number;
}

/**
 * Two thumbs on one track. They can meet but not cross. The inputs ignore the pointer and stay
 * for the keyboard: the track takes every press and gives it to the nearest thumb. Overlapping
 * thumbs are equally near, so there the drag direction picks, and neither can jam the other.
 */
export function rangeSlider(
  options: Range & {
    label: string;
    format(low: number, high: number): string;
    onInput(low: number, high: number): void;
  },
): RangeSlider {
  const lower = rangeInput(options, `${options.label} minimum`);
  const upper = rangeInput(options, `${options.label} maximum`);
  const output = h('output');
  const track = h('div', { class: 'track dual' }, h('i', { class: 'fill' }), lower, upper);

  const show = (low: number, high: number) => {
    track.style.setProperty('--lo', fraction(low, options));
    track.style.setProperty('--hi', fraction(high, options));
    setNumeric(output, options.format(low, high));
  };
  const onInput = (moved: HTMLInputElement) => {
    let low = lower.valueAsNumber;
    let high = upper.valueAsNumber;
    if (low > high) {
      if (moved === lower) lower.value = String((low = high));
      else upper.value = String((high = low));
    }
    options.onInput(low, high);
    show(low, high);
  };
  lower.addEventListener('input', () => onInput(lower));
  upper.addEventListener('input', () => onInput(upper));

  let drag: Drag | null = null;

  const moveThumb = ({ scale, offset }: Drag, thumb: HTMLInputElement, x: number) => {
    const before = thumb.value;
    thumb.value = String(scale.toValue(x + offset));
    if (thumb.value !== before) onInput(thumb);
  };
  const takeThumb = (active: Drag, thumb: HTMLInputElement) => {
    const thumbX = active.scale.toX(thumb.valueAsNumber);
    const grabbed = Math.abs(active.startX - thumbX) <= GRAB_PX;
    active.thumb = thumb;
    active.offset = grabbed ? thumbX - active.startX : 0;
    thumb.focus({ preventScroll: true });
    moveThumb(active, thumb, active.startX);
  };

  /** The thumb a press belongs to, or null if only the drag direction can tell. */
  const nearestThumb = (scale: Drag['scale'], x: number): HTMLInputElement | null => {
    const lowX = scale.toX(lower.valueAsNumber);
    const highX = scale.toX(upper.valueAsNumber);
    const lowGap = Math.abs(x - lowX);
    const highGap = Math.abs(x - highX);
    if (highX - lowX < THUMB_PX) {
      if (Math.min(lowGap, highGap) <= GRAB_PX) return null;
      return x < lowX ? lower : upper;
    }
    if (lowGap === highGap) return null;
    return lowGap < highGap ? lower : upper;
  };

  track.addEventListener('pointerdown', (event) => {
    if (event.button !== 0) return;
    track.setPointerCapture(event.pointerId);
    const scale = trackScale(track, options);
    const active: Drag = { scale, startX: event.clientX, thumb: null, offset: 0 };
    drag = active;
    const thumb = nearestThumb(scale, event.clientX);
    if (thumb) takeThumb(active, thumb);
  });
  // The chosen thumb has focus by now. Left alone, the press would move it to the body.
  track.addEventListener('mousedown', (event) => event.preventDefault());
  track.addEventListener('pointermove', (event) => {
    if (!drag) return;
    if (!drag.thumb) {
      const moved = event.clientX - drag.startX;
      if (Math.abs(moved) < DIRECTION_PX) return;
      takeThumb(drag, moved < 0 ? lower : upper);
    }
    if (drag.thumb) moveThumb(drag, drag.thumb, event.clientX);
  });
  const endDrag = () => (drag = null);
  track.addEventListener('pointerup', endDrag);
  track.addEventListener('pointercancel', endDrag);

  return {
    el: row(options.label, track, output),
    set(low, high) {
      if (drag) return;
      if (lower.valueAsNumber !== low) lower.value = String(low);
      if (upper.valueAsNumber !== high) upper.value = String(high);
      show(lower.valueAsNumber, upper.valueAsNumber);
    },
  };
}

export interface Segmented<T> {
  el: HTMLElement;
  set(value: T): void;
}

/**
 * One choice from a few, with arrow keys and a single tab stop. `role` picks the semantics:
 * `radio` for a setting, `tab` for the Settings tabs.
 */
export function segmented<T extends string>(options: {
  label: string;
  role: 'radio' | 'tab';
  className: string;
  choices: { value: T; label: string; icon?: string }[];
  onChange(value: T): void;
}): Segmented<T> {
  const selectedAttr = options.role === 'tab' ? 'aria-selected' : 'aria-checked';
  const buttons = options.choices.map((choice) => {
    const button = h('button', { type: 'button', role: options.role });
    if (choice.icon) {
      button.setAttribute('aria-label', choice.label);
      button.title = choice.label;
      button.append(icon(choice.icon));
    } else {
      button.textContent = choice.label;
    }
    return button;
  });

  const set = (value: T) => {
    options.choices.forEach((choice, index) => {
      const selected = choice.value === value;
      const button = buttons[index];
      button?.setAttribute(selectedAttr, String(selected));
      if (button) button.tabIndex = selected ? 0 : -1;
    });
  };
  const choose = (index: number) => {
    const choice = options.choices[index];
    if (!choice) return;
    set(choice.value);
    options.onChange(choice.value);
  };

  buttons.forEach((button, index) => {
    button.addEventListener('click', () => choose(index));
    button.addEventListener('keydown', (event) => {
      const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
      if (!step) return;
      event.preventDefault();
      const next = (index + step + buttons.length) % buttons.length;
      choose(next);
      buttons[next]?.focus();
    });
  });

  const el = h('div', {
    class: options.className,
    role: options.role === 'tab' ? 'tablist' : 'radiogroup',
    'aria-label': options.label,
  });
  el.append(...buttons);
  return { el, set };
}

export interface Toggle {
  el: HTMLButtonElement;
  set(on: boolean): void;
  setDisabled(disabled: boolean): void;
}

/** A switch. With `states` it spells out both positions, otherwise it is a plain knob. */
export function toggle(options: {
  label: string;
  states?: [off: string, on: string];
  onChange(on: boolean): void;
}): Toggle {
  const words = options.states?.map((word) => h('span', {}, word)) ?? [];
  const el = h(
    'button',
    {
      type: 'button',
      role: 'switch',
      class: options.states ? 'switch wide' : 'switch',
      'aria-label': options.label,
      'aria-checked': 'false',
    },
    h('i', { class: 'knob' }),
    ...words,
  );
  let on = false;
  el.addEventListener('click', () => options.onChange(!on));
  return {
    el,
    set(next) {
      if (next === on) return;
      on = next;
      el.setAttribute('aria-checked', String(on));
    },
    setDisabled(disabled) {
      if (el.disabled !== disabled) el.disabled = disabled;
    },
  };
}

/** A settings row holding a switch. */
export function switchRow(label: string, control: Toggle): HTMLElement {
  return h('div', { class: 'row switch-row' }, h('span', { class: 'label' }, label), control.el);
}
