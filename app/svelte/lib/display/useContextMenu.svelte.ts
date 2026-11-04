import { tick } from 'svelte';
import { ctxClamp } from './ui';

/** The right-click menu's state, owned by the component that shows it. */
export interface ContextMenu {
  /** The click the menu opened on; null while no menu is open. */
  anchor: { x: number; y: number } | null;
  /** Where the menu renders: the anchor, clamped into the viewport. */
  pos: { x: number; y: number };
  /** The rendered menu element, which the clamp measures. */
  el: HTMLDivElement | undefined;
}

/** One menu's state, reactive so a component `$effect` and the template can read it. */
export function contextMenu(): ContextMenu {
  const menu: ContextMenu = $state({ anchor: null, pos: { x: 0, y: 0 }, el: undefined });
  return menu;
}

/** Open the menu at a right-click. The anchor and its first position are the
 * same point until the clamp moves it, so they are set together here. */
export function openMenu(menu: ContextMenu, e: MouseEvent): void {
  e.preventDefault();
  const p = { x: e.clientX, y: e.clientY };
  menu.anchor = p;
  menu.pos = p;
}

export interface ContextMenuOptions {
  /** Close the menu (clear the anchor) plus any extra work. */
  close: () => void;
  /** Return true to keep the menu open for this click. */
  ignoreClick?: (e: MouseEvent) => boolean;
  /** Keep the click-away listener alive while true, even with no menu open. */
  extraActive?: () => boolean;
}

/**
 * The shared right-click menu lifecycle, installed from a component `$effect`:
 * the click-away / Escape listeners and the post-render viewport clamp. The
 * state stays component-owned (the template's reactivity needs to read it at
 * the top level), so this takes it and returns the cleanup.
 */
export function useContextMenu(
  menu: ContextMenu,
  opts: ContextMenuOptions,
): (() => void) | undefined {
  const ctx = menu.anchor;
  if (!ctx && !opts.extraActive?.()) return;
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === 'Escape') opts.close();
  };
  const onClick = (e: MouseEvent): void => {
    if (opts.ignoreClick?.(e)) return;
    opts.close();
  };
  document.addEventListener('click', onClick);
  document.addEventListener('keydown', onKey);
  void tick().then(() => {
    const el = menu.el;
    if (!ctx || !el) return;
    menu.pos = ctxClamp(ctx, el);
  });
  return () => {
    document.removeEventListener('click', onClick);
    document.removeEventListener('keydown', onKey);
  };
}
