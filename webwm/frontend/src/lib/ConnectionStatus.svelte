<!--
  ConnectionStatus.svelte — compact bridge channel health indicator.

  Shows three dots (render / input / control) with animated states.
  Clicking expands a popover with channel URLs + reconnect options.
-->
<script>
  import { channels } from './ws.js';

  // Extract the individual status stores so Svelte's $ auto-subscribe works.
  // channels is a plain object; channels.render.status etc. are writable stores.
  const renderStatus  = channels.render.status;
  const inputStatus   = channels.input.status;
  const controlStatus = channels.control.status;

  const defs = [
    { key: 'render',  port: '7700', label: 'Render'  },
    { key: 'input',   port: '7701', label: 'Input'   },
    { key: 'control', port: '7702', label: 'Control' },
  ];

  let expanded = false;

  const stateIcon = {
    open:       'circle',
    connecting: 'sync',
    closed:     'radio_button_unchecked',
    error:      'error',
  };

  const stateColor = {
    open:       'var(--md-sys-color-primary)',
    connecting: 'var(--md-sys-color-tertiary)',
    closed:     'var(--md-sys-color-outline)',
    error:      'var(--md-sys-color-error)',
  };

  // Rebuild statuses whenever any of the three stores changes.
  $: statuses = [
    { ...defs[0], status: $renderStatus  },
    { ...defs[1], status: $inputStatus   },
    { ...defs[2], status: $controlStatus },
  ];

  $: allOpen  = statuses.every(s => s.status === 'open');
  $: anyError = statuses.some(s => s.status === 'error' || s.status === 'closed');
</script>

<div class="status-root" class:expanded>
  <!-- Collapsed: single pill summary -->
  <button
    class="pill"
    class:ok={allOpen}
    class:bad={anyError && !allOpen}
    title="Bridge connection status"
    aria-label="Bridge connection: {allOpen ? 'all connected' : 'degraded'}"
    aria-expanded={expanded}
    on:click={() => expanded = !expanded}
  >
    {#each statuses as s}
      <span
        class="dot icon"
        class:spin={s.status === 'connecting'}
        style="color: {stateColor[s.status]}; font-size: 10px; font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 20"
        aria-hidden="true"
      >{stateIcon[s.status]}</span>
    {/each}
    <span class="pill-label">{allOpen ? 'Connected' : anyError ? 'Disconnected' : 'Connecting…'}</span>
  </button>

  <!-- Expanded: per-channel detail card -->
  {#if expanded}
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
    <div
      class="detail-card"
      role="status"
      aria-label="Channel details"
      on:click|stopPropagation
    >
      {#each statuses as s}
        <div class="channel-row">
          <span
            class="row-icon icon"
            class:spin={s.status === 'connecting'}
            style="color: {stateColor[s.status]}; font-size: 14px; font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 20"
            aria-hidden="true"
          >{stateIcon[s.status]}</span>
          <span class="row-label">{s.label}</span>
          <span class="row-port">:{s.port}</span>
          <span class="row-status" style="color: {stateColor[s.status]}">{s.status}</span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<!-- Close detail card on backdrop click -->
{#if expanded}
  <div class="backdrop" aria-hidden="true" on:click={() => expanded = false}></div>
{/if}

<style>
  .status-root {
    position: relative;
    display: flex;
    align-items: center;
  }

  .pill {
    display: flex;
    align-items: center;
    gap: 3px;
    padding: 4px 10px 4px 8px;
    border: none;
    border-radius: var(--md-sys-shape-corner-full);
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface-variant);
    cursor: pointer;
    font-family: var(--font-family);
    font-size: var(--font-size-label);
    font-weight: var(--font-weight-medium);
    gap: 5px;
    letter-spacing: 0.01em;
    transition: background var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .pill:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  .pill-label {
    font-size: var(--font-size-label);
    white-space: nowrap;
  }

  /* Detail popover */
  .detail-card {
    position: absolute;
    bottom: calc(100% + 8px);
    right: 0;
    min-width: 220px;
    background: var(--md-sys-color-surface-container-high);
    border-radius: var(--md-sys-shape-corner-medium);
    box-shadow: var(--md-sys-elevation-3);
    border: 1px solid var(--md-sys-color-outline-variant);
    padding: 8px 0;
    z-index: 200;
  }

  .channel-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    font-size: var(--font-size-body-sm);
  }

  .row-icon { flex-shrink: 0; }

  .row-label {
    flex: 1;
    font-weight: var(--font-weight-medium);
    color: var(--md-sys-color-on-surface);
  }

  .row-port {
    color: var(--md-sys-color-outline);
    font-family: monospace;
  }

  .row-status {
    font-size: var(--font-size-label);
    font-weight: var(--font-weight-medium);
    text-transform: capitalize;
  }

  /* Spinning animation for "connecting" state */
  @keyframes spin { to { transform: rotate(360deg); } }
  .spin { animation: spin 1s linear infinite; display: inline-block; }

  /* Backdrop */
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 190;
  }
</style>
