<!--
  DwindleLayout.svelte — Dwindle tiling layout (Caelestia-style).

  0 windows → empty workspace hint
  1 window  → fills workspace
  2+ windows → primary (left, golden ratio) + secondary stack (right)
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
      <span class="empty-icon" aria-hidden="true">blur_on</span>
      <span class="empty-label">No open windows</span>
      <span class="empty-hint">Open an app from the launcher</span>
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
          <span class="ph-icon icon" aria-hidden="true">{primary.icon ?? 'apps'}</span>
          <span class="ph-title">{primary.title}</span>
          <span class="ph-sub">App surface · compositor not yet connected</span>
        </div>
      </div>
    </div>

  {:else}
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
          <span class="ph-icon icon" aria-hidden="true">{primary.icon ?? 'apps'}</span>
          <span class="ph-title">{primary.title}</span>
          <span class="ph-sub">App surface · compositor not yet connected</span>
        </div>
      </div>
    </div>

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
              <span class="ph-icon icon" aria-hidden="true">{win.icon ?? 'apps'}</span>
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
    width:   100%;
    height:  100%;
    gap:     10px;
    padding: 10px;
  }

  /* ── Empty workspace ──────────────────────────────────────────────────── */
  .empty-workspace {
    flex:           1;
    display:        flex;
    flex-direction: column;
    align-items:    center;
    justify-content: center;
    gap:            12px;
  }

  .empty-icon {
    font-family: 'Material Symbols Rounded';
    font-size:   64px;
    font-variation-settings: 'FILL' 0, 'wght' 200, 'GRAD' -25, 'opsz' 48;
    color:       var(--ctp-surface2);
    line-height: 1;
    user-select: none;
  }

  .empty-label {
    font-size:   var(--font-size-title-sm);
    font-weight: var(--font-weight-medium);
    color:       var(--ctp-overlay1);
  }

  .empty-hint {
    font-size: var(--font-size-body-sm);
    color:     var(--ctp-overlay0);
  }

  /* ── Tiles ────────────────────────────────────────────────────────────── */
  .single  { flex: 1; }
  .primary { flex: 1.618; }

  .stack {
    display:        flex;
    flex-direction: column;
    flex:           1;
    gap:            10px;
    min-width:      0;
  }

  .stack .tile { flex: 1; }

  .tile {
    display:        flex;
    flex-direction: column;
    border-radius:  var(--md-sys-shape-corner-large);
    overflow:       hidden;
    cursor:         default;

    /* Glassmorphism tile surface */
    background: color-mix(in srgb, var(--ctp-mantle) 90%, transparent);
    backdrop-filter:         blur(16px);
    -webkit-backdrop-filter: blur(16px);
    border: 1px solid color-mix(in srgb, var(--ctp-surface2) 55%, transparent);
    box-shadow: 0 4px 20px rgba(0,0,0,0.40);

    transition:
      border-color var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-standard),
      box-shadow   var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-standard);
  }

  .tile.focused {
    border-color: color-mix(in srgb, var(--ctp-mauve) 50%, var(--ctp-surface2));
    box-shadow:
      0 4px 24px rgba(0,0,0,0.45),
      0 0 0 1px color-mix(in srgb, var(--ctp-mauve) 35%, transparent);
  }

  /* ── Window surface placeholder ───────────────────────────────────────── */
  .surface {
    flex:            1;
    display:         flex;
    align-items:     center;
    justify-content: center;
    background:      color-mix(in srgb, var(--ctp-crust) 85%, transparent);
    position:        relative;
    overflow:        hidden;
  }

  /* Subtle dot-grid texture */
  .surface::before {
    content:     '';
    position:    absolute;
    inset:       0;
    background-image: radial-gradient(circle, var(--ctp-surface1) 1px, transparent 1px);
    background-size: 24px 24px;
    opacity:     0.18;
    pointer-events: none;
  }

  .ph-content {
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            10px;
    position:       relative;
  }

  .ph-icon {
    font-size: 48px;
    font-variation-settings: 'FILL' 1, 'wght' 200, 'GRAD' -25, 'opsz' 48;
    color:     var(--ctp-surface2);
    line-height: 1;
    user-select: none;
  }

  .ph-title {
    font-size:   var(--font-size-body);
    font-weight: var(--font-weight-medium);
    color:       var(--ctp-overlay1);
  }

  .ph-sub {
    font-size: var(--font-size-body-sm);
    color:     var(--ctp-overlay0);
  }
</style>
