<!--
  Titlebar.svelte — Caelestia-style window chrome.

  macOS-style traffic-light dots for window controls.
  Minimal height, pill-shaped identity area on focus.

  Props:
    title    string   — window title
    icon     string   — Material Symbols icon name
    appId    number   — numeric app id
    focused  boolean  — focus state
-->
<script>
  import '@material/web/iconbutton/icon-button.js';
  import { closeWindow, minimizeWindow } from './windows.js';

  export let title   = 'Untitled';
  export let icon    = 'apps';
  export let appId   = 0;
  export let focused = false;

  function close()    { closeWindow(appId); }
  function minimize() { minimizeWindow(appId); }
  function maximize() {}
</script>

<header class="titlebar" class:focused aria-label="Window: {title}">
  <!-- Traffic-light dots -->
  <div class="dots" role="group" aria-label="Window controls">
    <button
      class="dot dot-close"
      title="Close"
      aria-label="Close {title}"
      on:click={close}
    ></button>
    <button
      class="dot dot-minimize"
      title="Minimize"
      aria-label="Minimize {title}"
      on:click={minimize}
    ></button>
    <button
      class="dot dot-maximize"
      title="Maximize"
      aria-label="Maximize {title}"
      on:click={maximize}
    ></button>
  </div>

  <!-- App identity -->
  <div class="identity">
    <span class="app-icon icon" aria-hidden="true">{icon}</span>
    <span class="title">{title}</span>
  </div>
</header>

<style>
  .titlebar {
    display:      flex;
    align-items:  center;
    height:       36px;
    padding:      0 10px;
    gap:          10px;
    background:   color-mix(in srgb, var(--ctp-mantle) 95%, transparent);
    color:        var(--ctp-subtext0);
    user-select:  none;
    border-radius:
      var(--md-sys-shape-corner-large)
      var(--md-sys-shape-corner-large)
      0 0;
    border-bottom: 1px solid color-mix(in srgb, var(--ctp-surface2) 60%, transparent);
  }

  .titlebar.focused {
    background: color-mix(in srgb, var(--ctp-surface0) 90%, transparent);
    color:      var(--ctp-text);
    border-bottom-color: color-mix(in srgb, var(--ctp-mauve) 20%, var(--ctp-surface2));
  }

  /* ── Traffic-light dots ────────────────────────────────────────────────── */
  .dots {
    display:     flex;
    align-items: center;
    gap:         6px;
    flex-shrink: 0;
  }

  .dot {
    width:         12px;
    height:        12px;
    border-radius: 50%;
    border:        none;
    cursor:        pointer;
    padding:       0;
    /* Dim when window not focused */
    opacity:       0.35;
    transition:
      opacity    var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      filter     var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      transform  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .dot-close    { background: #ff5f57; }
  .dot-minimize { background: #ffbd2e; }
  .dot-maximize { background: #28c840; }

  /* Show at full brightness when titlebar is focused */
  .focused .dot { opacity: 1; }

  /* Hover: brighten and scale up */
  .dots:hover .dot { opacity: 0.85; }
  .dot:hover { opacity: 1 !important; filter: brightness(1.15); transform: scale(1.1); }
  .dot:active { transform: scale(0.9); }

  /* ── Identity ──────────────────────────────────────────────────────────── */
  .identity {
    display:    flex;
    align-items: center;
    gap:        6px;
    flex:       1;
    min-width:  0;
  }

  .app-icon {
    font-size: 15px;
    color:     var(--ctp-mauve);
    flex-shrink: 0;
    font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 20;
    opacity: 0.7;
    transition: opacity var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .focused .app-icon { opacity: 1; }

  .title {
    font-size:    var(--font-size-label-lg);
    font-weight:  var(--font-weight-medium);
    white-space:  nowrap;
    overflow:     hidden;
    text-overflow: ellipsis;
    letter-spacing: 0.01em;
    color:        var(--ctp-subtext0);
    transition:   color var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .focused .title { color: var(--ctp-text); }
</style>
