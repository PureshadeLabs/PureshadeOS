<!--
  Titlebar.svelte — M3 window chrome.

  Uses @material/web <md-icon-button> for action buttons — real ripple,
  state layer, and keyboard accessibility.

  Props:
    title    string   — window title
    icon     string   — Material Symbols icon name (default: 'apps')
    appId    number   — numeric app id
    focused  boolean  — keyboard/pointer focus on this window
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
  // maximize: placeholder — real impl needs layout engine
  function maximize() {}
</script>

<header class="titlebar" class:focused aria-label="Window: {title}">
  <!-- App identity -->
  <div class="identity">
    <span class="app-icon icon" aria-hidden="true">{icon}</span>
    <span class="title">{title}</span>
  </div>

  <!-- Window controls -->
  <div class="controls" role="group" aria-label="Window controls">
    <md-icon-button
      title="Minimize"
      aria-label="Minimize {title}"
      on:click={minimize}
    >
      <span class="icon" style="font-size:18px">minimize</span>
    </md-icon-button>

    <md-icon-button
      title="Maximize"
      aria-label="Maximize {title}"
      on:click={maximize}
    >
      <span class="icon" style="font-size:18px">crop_square</span>
    </md-icon-button>

    <md-icon-button
      class="close-btn"
      title="Close"
      aria-label="Close {title}"
      on:click={close}
    >
      <span class="icon" style="font-size:18px">close</span>
    </md-icon-button>
  </div>
</header>

<style>
  .titlebar {
    display: flex;
    align-items: center;
    height:  40px;
    padding: 0 4px 0 12px;
    gap: 4px;
    background: var(--md-sys-color-surface-container);
    color: var(--md-sys-color-on-surface-variant);
    user-select: none;
    /* Top rounded corners match the tile's border-radius */
    border-radius:
      var(--md-sys-shape-corner-medium)
      var(--md-sys-shape-corner-medium)
      0 0;
    border-bottom: 1px solid var(--md-sys-color-outline-variant);
    position: relative;
  }

  /* Focused: primary-tinted surface + left accent stripe */
  .titlebar.focused {
    background: color-mix(
      in srgb,
      var(--md-sys-color-primary) 8%,
      var(--md-sys-color-surface-container-high)
    );
    color: var(--md-sys-color-on-surface);
    border-bottom-color: color-mix(
      in srgb,
      var(--md-sys-color-primary) 30%,
      var(--md-sys-color-outline-variant)
    );
  }

  /* Accent stripe on the left edge when focused */
  .titlebar.focused::before {
    content: '';
    position: absolute;
    left: 0;
    top: 6px;
    bottom: 6px;
    width: 3px;
    border-radius: 0 2px 2px 0;
    background: var(--md-sys-color-primary);
  }

  .identity {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
  }

  .app-icon {
    font-size: 18px;
    color: var(--md-sys-color-primary);
    flex-shrink: 0;
    font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 20;
  }

  .title {
    font-size: var(--font-size-title-sm);
    font-weight: var(--font-weight-medium);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    letter-spacing: 0.01em;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 0;
    flex-shrink: 0;
  }

  /* Size the MWC icon buttons down to 32px for a compact titlebar */
  .controls md-icon-button {
    --md-icon-button-container-width:  32px;
    --md-icon-button-container-height: 32px;
    --md-icon-button-icon-size:        18px;
    --md-icon-button-icon-color:            var(--md-sys-color-on-surface-variant);
    --md-icon-button-hover-icon-color:      var(--md-sys-color-on-surface);
    --md-icon-button-hover-state-layer-color: var(--md-sys-color-on-surface-variant);
    --md-icon-button-pressed-icon-color:    var(--md-sys-color-on-surface);
  }

  /* Close button — error color on hover */
  .controls :global(.close-btn) {
    --md-icon-button-hover-icon-color:        var(--md-sys-color-error);
    --md-icon-button-hover-state-layer-color: var(--md-sys-color-error);
  }
</style>
