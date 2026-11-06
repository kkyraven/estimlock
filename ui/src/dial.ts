import { percent } from './status';
import type { Snapshot } from './types';

const SVG = 'http://www.w3.org/2000/svg';
const TEETH = 50;
const SIZE = 300;
const CENTRE = SIZE / 2;
const ROOT = 112;
const TIP = 130;
// Clockwise from lower left, leaving the bottom open.
const START_DEGREES = 135;
const SWEEP_DEGREES = 270;

function polar(radius: number, degrees: number): string {
  const radians = (degrees * Math.PI) / 180;
  const x = CENTRE + radius * Math.cos(radians);
  const y = CENTRE + radius * Math.sin(radians);
  return `${x.toFixed(2)} ${y.toFixed(2)}`;
}

function angle(level: number): number {
  return START_DEGREES + SWEEP_DEGREES * level;
}

function svgElement(name: string, attrs: Record<string, string>): SVGElement {
  const element = document.createElementNS(SVG, name);
  for (const [key, value] of Object.entries(attrs)) element.setAttribute(key, value);
  return element;
}

/** One sawtooth: a slow rise to the tip, then a sheer face back to the root. */
function tooth(index: number): SVGElement {
  const step = SWEEP_DEGREES / TEETH;
  const from = START_DEGREES + index * step;
  const to = from + step * 0.86;
  return svgElement('path', { d: `M${polar(ROOT, from)}L${polar(TIP, to)}L${polar(ROOT, to)}Z` });
}

function toothCount(level: number): number {
  return Math.round(level * TEETH);
}

export interface Dial {
  el: SVGElement;
  /** `known` says whether `output` is a real level, see `hasLevel`. */
  update(snapshot: Snapshot, known: boolean): void;
}

/**
 * The ring of teeth, with a pawl for base and a stop for the ceiling. Teeth are lit up to the
 * output: `lit` to base, `hot` above it, `owed` for the gap below base while recovering, and
 * `dead` past the ceiling. Nothing here animates, it only changes when a snapshot does.
 */
export function createDial(): Dial {
  const el = svgElement('svg', {
    class: 'dial',
    viewBox: `0 0 ${SIZE} ${SIZE - 36}`,
    role: 'img',
  });
  const teeth = Array.from({ length: TEETH }, (_, index) => tooth(index));
  const classes: string[] = teeth.map(() => '');
  const stop = svgElement('path', {
    class: 'stop',
    d: `M${polar(ROOT - 8, 0)}L${polar(TIP + 8, 0)}`,
  });
  const pawl = svgElement('path', {
    class: 'pawl',
    d: `M${polar(TIP + 2, 0)}L${polar(TIP + 14, -3.2)}L${polar(TIP + 14, 3.2)}Z`,
  });
  el.append(...teeth, stop, pawl);

  let pawlAt = '';
  let stopAt = '';
  let spoken = '';
  const place = (marker: SVGElement, level: number, was: string): string => {
    const transform = `rotate(${angle(level).toFixed(2)} ${CENTRE} ${CENTRE})`;
    if (transform !== was) marker.setAttribute('transform', transform);
    return transform;
  };

  return {
    el,
    update(snapshot, known) {
      const { base, on, persisted } = snapshot;
      const { ceiling } = persisted.config;
      const lit = known ? toothCount(snapshot.output) : 0;
      const baseTooth = toothCount(base);
      const ceilingTooth = toothCount(ceiling);

      teeth.forEach((element, index) => {
        let name: string;
        if (index < lit) name = on && index >= baseTooth ? 'hot' : 'lit';
        else if (index >= ceilingTooth) name = 'dead';
        else if (on && index < baseTooth) name = 'owed';
        else name = 'idle';
        if (classes[index] !== name) {
          classes[index] = name;
          element.setAttribute('class', name);
        }
      });

      pawlAt = place(pawl, base, pawlAt);
      stopAt = place(stop, ceiling, stopAt);
      // 0 means base was never set, so there is nothing to mark.
      pawl.toggleAttribute('hidden', base <= 0);

      const words = known
        ? `Volume ${percent(snapshot.output)}, base ${percent(base)}, ceiling ${percent(ceiling)}`
        : 'No volume';
      if (words !== spoken) {
        spoken = words;
        el.setAttribute('aria-label', words);
      }
    },
  };
}
