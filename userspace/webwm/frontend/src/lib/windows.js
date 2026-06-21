/**
 * windows.js — reactive window list store.
 *
 * Single source of truth for all open windows.  Components import the store
 * and the four action functions directly — no prop drilling, no events.
 *
 * When the bridge is wired up, `openWindow` / `closeWindow` can also fire
 * SYS_EXEC / SYS_TASK_EXIT commands over the control channel in addition to
 * updating local state.
 */

import { writable, derived } from 'svelte/store';

let _nextId = 1;

function makeWindow(title, icon = 'apps') {
  return { id: _nextId++, title, icon, focused: false, minimized: false };
}

// Start with two placeholder windows so the shell looks populated by default.
const _initial = [
  { ...(makeWindow('Terminal',     'terminal')),    focused: true  },
  { ...(makeWindow('File Manager', 'folder_open')), focused: false },
];

export const windows = writable(_initial);

// Derived store — only non-minimized windows (fed to DwindleLayout).
export const visibleWindows = derived(windows, $w =>
  $w.filter(w => !w.minimized)
);

// ── Actions ──────────────────────────────────────────────────────────────────

/**
 * Open a new window and focus it.
 */
export function openWindow(title, icon = 'apps') {
  windows.update(wins => {
    const w = makeWindow(title, icon);
    w.focused = true;
    // Unfocus everything else.
    return [...wins.map(x => ({ ...x, focused: false })), w];
  });
}

/**
 * Close a window by id.  Auto-focuses the last remaining visible window.
 */
export function closeWindow(id) {
  windows.update(wins => {
    const remaining = wins.filter(w => w.id !== id);
    // If nothing is focused anymore, focus the last visible window.
    const visible = remaining.filter(w => !w.minimized);
    if (visible.length > 0 && !visible.some(w => w.focused)) {
      const last = visible[visible.length - 1];
      return remaining.map(w => ({ ...w, focused: w.id === last.id }));
    }
    return remaining;
  });
}

/**
 * Bring a window to front (focus it, unfocus all others).
 */
export function focusWindow(id) {
  windows.update(wins =>
    wins.map(w => ({ ...w, focused: w.id === id, minimized: w.id === id ? false : w.minimized }))
  );
}

/**
 * Toggle minimized state.  Minimizing the focused window auto-focuses the next.
 */
export function minimizeWindow(id) {
  windows.update(wins => {
    const updated = wins.map(w =>
      w.id === id ? { ...w, minimized: true, focused: false } : w
    );
    // If nothing focused, pick the last visible.
    const visible = updated.filter(w => !w.minimized);
    if (visible.length > 0 && !visible.some(w => w.focused)) {
      const last = visible[visible.length - 1];
      return updated.map(w => ({ ...w, focused: w.id === last.id }));
    }
    return updated;
  });
}
