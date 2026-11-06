type Child = Node | string;

/** Creates an element. String props become attributes, `class` included. */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string> = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  for (const [name, value] of Object.entries(attrs)) element.setAttribute(name, value);
  element.append(...children);
  return element;
}

/** Parses a Phosphor SVG imported with `?raw`. Colour comes from `currentColor`. */
export function icon(svg: string): Element {
  const holder = document.createElement('span');
  holder.className = 'icon';
  holder.setAttribute('aria-hidden', 'true');
  holder.innerHTML = svg;
  return holder;
}

// Snapshots arrive 20 times a second. These skip the DOM write when nothing changed.

export function setText(element: Element, text: string): void {
  if (element.textContent !== text) element.textContent = text;
}

/**
 * Like `setText`, with each digit in a fixed-width cell. Saira has no tabular figures, so
 * `font-variant-numeric` does nothing and a changing number would otherwise jitter.
 */
export function setNumeric(element: HTMLElement, text: string): void {
  if (element.textContent === text) return;
  const parts = text.split(/(\d)/).filter((part) => part !== '');
  const cells = parts.map((part) => (/\d/.test(part) ? h('span', { class: 'digit' }, part) : part));
  // One wrapper, so a flex parent sees a single item and not one per digit.
  element.replaceChildren(h('span', {}, ...cells));
}

export function setFlag(element: Element, name: string, on: boolean): void {
  element.toggleAttribute(name, on);
}

export function setDisabled(element: HTMLButtonElement, disabled: boolean): void {
  if (element.disabled !== disabled) element.disabled = disabled;
}

export function setHidden(element: HTMLElement, hidden: boolean): void {
  if (element.hidden !== hidden) element.hidden = hidden;
}

/** Runs `run` at once, then at most every `ms`, always ending with the latest call. */
export function throttle<A extends unknown[]>(run: (...args: A) => void, ms: number) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let latest: A | undefined;
  const fire = () => {
    timer = undefined;
    if (latest) {
      const args = latest;
      latest = undefined;
      run(...args);
      timer = setTimeout(fire, ms);
    }
  };
  return (...args: A) => {
    latest = args;
    if (timer === undefined) fire();
  };
}
