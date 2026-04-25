<!--
  DwindleLayout.svelte — Dwindle tiling layout.

  0 windows → empty workspace hint
  1 window  → fills entire workspace
  2+ windows → primary (golden-ratio left) + secondary stack (right)

  Click on any tile to focus it.
-->
<script>
  import Titlebar from './Titlebar.svelte';
  import { focusWindow } from './windows.js';

  export let windows = [];

  $: [primary, ...secondary] = windows;
</script>

<div class="dwindle" role="region" aria-label="Tiled windows">

  {#if windows.length === 0}
    <div class="empty-workspace">
      <span class="empty-icon" aria-hidden="true">dashboard</span>
      <span class="empty-label">No open windows</span>
      <span class="empty-hint">Open an app from the launcher ⊞</span>
    </div>

  {:else if windows.length === 1}
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
    <div
      class="tile single focused"
      role="region"
      aria-label={primary.title}
      on:click={() => focusWindow(primary.id)}
    >
      <Titlebar title={primary.title} icon={primary.icon ?? 'apps'} appId={primary.id} focused={true} />
      <div class="surface">
        <div class="ph-content">
          <span class="ph-icon" aria-hidden="true">{primary.icon ?? 'apps'}</span>
          <span class="ph-title">{primary.title}</span>
          <span class="ph-sub">App surface · compositor not yet connected</span>
        </div>
      </div>
    </div>

  {:else}
    <!-- Primary tile (left) -->
    <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
    <div
      class="tile primary"
      class:focused={primary.focused}
      role="region"
      aria-label={primary.title}
      on:click={() => focusWindow(primary.id)}
    >
      <Titlebar title={primary.title} icon={primary.icon ?? 'apps'} appId={primary.id} focused={primary.focused} />
      <div class="surface">
        <div class="ph-content">
          <span class="ph-icon" aria-hidden="true">{primary.icon ?? 'apps'}</span>
          <span class="ph-title">{primary.title}</span>
          <span class="ph-sub">App surface · compositor not yet connected</span>
        </div>
      </div>
    </div>

    <!-- Secondary stack (right) -->
    <div class="stack">
      {#each secondary as win (win.id)}
        <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
        <div
          class="tile"
          class:focused={win.focused}
          role="region"
          aria-label={win.title}
          on:click={() => focusWindow(win.id)}
        >
          <Titlebar title={win.title} icon={win.icon ?? 'apps'} appId={win.id} focused={win.focused} />
          <div class="surface">
            <div class="ph-content">
              <span class="ph-icon" aria-hidden="true">{win.icon ?? 'apps'}</span>
              <span class="ph-title">{win.title}</span>
              <span class="ph-sub">App surface · compositor not yet connected</span>
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

</div>

<style>
  .dwindle {
    display: flex;
    width:  100%;
    height: 100%;
    gap: 8px;
    padding: 8px;
  }

  /* ── Empty workspace ──────────────────────────────────────────────────── */
  .empty-workspace {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
  }

  .empty-icon {
    font-family: 'Material Symbols Rounded';
    font-size: 72px;
    font-variation-settings: 'FILL' 0, 'wght' 300, 'GRAD' -25, 'opsz' 48;
    color: var(--md-sys-color-outline-variant);
    opacity: 0.35;
    line-height: 1;
    user-select: none;
  }

  .empty-label {
    font-size: var(--font-size-title);
    font-weight: var(--font-weight-medium);
    color: var(--md-sys-color-on-surface-variant);
    opacity: 0.45;
  }

  .empty-hint {
    font-size: var(--font-size-body-sm);
    color: var(--md-sys-color-outline);
    opacity: 0.45;
  }

  /* ── Tiles ────────────────────────────────────────────────────────────── */
  .single  { flex: 1; }
  .primary { flex: 1.618; }

  .stack {
    display: flex;
    flex-direction: column;
    flex: 1;
    gap: 8px;
    min-width: 0;
  }

  .stack .tile { flex: 1; }

  .tile {
    display: flex;
    flex-direction: column;
    background: var(--md-sys-color-surface-container-low);
    border-radius: var(--md-sys-shape-corner-medium);
    overflow: hidden;
    box-shadow: var(--md-sys-elevation-1);
    border: 1px solid var(--md-sys-color-outline-variant);
    cursor: default;
    transition:
      box-shadow   var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-standard),
      border-color var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-standard);
  }

  .tile.focused {
    border-color: var(--md-sys-color-primary);
    box-shadow: var(--md-sys-elevation-3), 0 0 0 1px var(--md-sys-color-primary);
  }

  /* ── Window surface (placeholder) ────────────────────────────────────── */
  .surface {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--md-sys-color-surface-container-lowest);
    position: relative;
    overflow: hidden;
  }

  .surface::before {
    content: '';
    position: absolute;
    inset: 0;
    background-image:
      linear-gradient(var(--md-sys-color-outline-variant) 1px, transparent 1px),
      linear-gradient(90deg, var(--md-sys-color-outline-variant) 1px, transparent 1px);
    background-size: 24px 24px;
    opacity: 0.05;
    pointer-events: none;
  }

  .ph-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    position: relative;
  }

  .ph-icon {
    font-family: 'Material Symbols Rounded';
    font-size: 52px;
    font-variation-settings: 'FILL' 1, 'wght' 300, 'GRAD' -25, 'opsz' 48;
    color: var(--md-sys-color-outline-variant);
    opacity: 0.4;
    line-height: 1;
    user-select: none;
  }

  .ph-title {
    font-size: var(--font-size-body-lg);
    font-weight: var(--font-weight-medium);
    color: var(--md-sys-color-on-surface-variant);
    opacity: 0.5;
  }

  .ph-sub {
    font-size: var(--font-size-body-sm);
    color: var(--md-sys-color-outline);
    opacity: 0.4;
  }
</style>
