<!--
  Sidebar.svelte — Caelestia-style left vertical strip.

  Top section    : logo (opens OSD), quick-action icons, workspace dots
  Middle section : rotated workspace name label
  Bottom section : system tray — clock, connection, power
-->
<script>
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';
  import ConnectionStatus from './ConnectionStatus.svelte';
  import { windows } from './windows.js';

  const dispatch = createEventDispatcher();

  export let osdOpen      = false;
  export let launcherOpen = false;

  let hour   = pad(new Date().getHours());
  let minute = pad(new Date().getMinutes());
  let timer;

  function pad(n) { return String(n).padStart(2, '0'); }

  onMount(() => {
    timer = setInterval(() => {
      hour   = pad(new Date().getHours());
      minute = pad(new Date().getMinutes());
    }, 15_000);
  });

  onDestroy(() => clearInterval(timer));

  // How many windows open — used for workspace dot density
  $: winCount = $windows.length;
</script>

<aside class="sidebar" aria-label="Sidebar">

  <!-- ── Top section ───────────────────────────────────────────────────── -->
  <div class="section top">

    <!-- Logo / OSD trigger -->
    <button
      class="icon-btn logo-btn"
      class:active={osdOpen}
      title="Open dashboard"
      aria-label="Open dashboard panel"
      aria-expanded={osdOpen}
      on:click={() => dispatch('toggleOSD')}
    >
      <!-- RaptorOS Λ glyph -->
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
        <path d="M9 2L16 16H2L9 2Z" stroke="currentColor" stroke-width="1.8"
              stroke-linejoin="round" fill="none"/>
        <path d="M5.5 12.5H12.5" stroke="currentColor" stroke-width="1.4"
              stroke-linecap="round"/>
      </svg>
    </button>

    <!-- Launcher trigger -->
    <button
      class="icon-btn"
      class:active={launcherOpen}
      title="App launcher"
      aria-label="Open app launcher"
      aria-expanded={launcherOpen}
      on:click={() => dispatch('toggleLauncher')}
    >
      <span class="icon" style="font-size:15px;font-variation-settings:'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 20">
        {launcherOpen ? 'close' : 'grid_view'}
      </span>
    </button>

    <!-- Screenshot placeholder -->
    <button class="icon-btn" title="Screenshot" aria-label="Take screenshot">
      <span class="icon" style="font-size:15px">screenshot</span>
    </button>

    <!-- Workspace dots -->
    <div class="workspace-dots" aria-label="Workspaces" role="list">
      <div class="dot active" role="listitem" aria-label="Workspace 1 (active)"></div>
      {#if winCount > 0}
        <div class="dot" role="listitem" aria-label="Workspace 2"></div>
      {/if}
    </div>

  </div>

  <!-- ── Middle: workspace name ─────────────────────────────────────────── -->
  <div class="section middle">
    <span class="workspace-label" aria-label="Current workspace: Desktop">Desktop</span>
  </div>

  <!-- ── Bottom: system tray ───────────────────────────────────────────── -->
  <div class="section bottom">

    <ConnectionStatus compact />

    <button class="icon-btn" title="Calendar" aria-label="Calendar">
      <span class="icon" style="font-size:15px">calendar_month</span>
    </button>

    <!-- Clock — hour stacked over minute -->
    <div class="clock" aria-label="Current time {hour}:{minute}">
      <span class="clock-hour">{hour}</span>
      <span class="clock-sep">·</span>
      <span class="clock-min">{minute}</span>
    </div>

    <button class="icon-btn power-btn" title="Power" aria-label="Power menu">
      <span class="icon" style="font-size:15px">power_settings_new</span>
    </button>

  </div>

</aside>

<style>
  .sidebar {
    width:          44px;
    min-width:      44px;
    height:         100%;
    display:        flex;
    flex-direction: column;
    align-items:    center;
    flex-shrink:    0;
    position:       relative;
    z-index:        20;
    overflow:       hidden;

    background: color-mix(in srgb, var(--ctp-mantle) 88%, transparent);
    backdrop-filter:         blur(24px) saturate(140%);
    -webkit-backdrop-filter: blur(24px) saturate(140%);

    /* Right-edge accent line */
    border-right: 1px solid color-mix(in srgb, var(--ctp-surface2) 55%, transparent);
  }

  /* ── Sections ───────────────────────────────────────────────────────── */
  .section {
    width:          100%;
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            4px;
    padding:        8px 0;
  }

  .top    { flex-shrink: 0; }
  .middle { flex: 1; justify-content: center; }
  .bottom { flex-shrink: 0; padding-bottom: 10px; }

  /* ── Icon buttons ───────────────────────────────────────────────────── */
  .icon-btn {
    display:         flex;
    align-items:     center;
    justify-content: center;
    width:           32px;
    height:          32px;
    border:          none;
    border-radius:   var(--md-sys-shape-corner-small);
    background:      transparent;
    color:           var(--ctp-subtext0);
    cursor:          pointer;
    flex-shrink:     0;
    transition:
      background  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      color       var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .icon-btn:hover {
    background: color-mix(in srgb, var(--ctp-surface0) 80%, transparent);
    color:      var(--ctp-text);
  }

  .icon-btn.active {
    background: color-mix(in srgb, var(--ctp-mauve) 22%, transparent);
    color:      var(--ctp-mauve);
    border-radius: var(--md-sys-shape-corner-full);
  }

  /* Logo button — slightly larger */
  .logo-btn {
    width:  34px;
    height: 34px;
    border-radius: var(--md-sys-shape-corner-full);
    margin-bottom: 2px;
  }

  .logo-btn.active {
    background: color-mix(in srgb, var(--ctp-mauve) 28%, transparent);
    box-shadow: 0 0 10px color-mix(in srgb, var(--ctp-mauve) 30%, transparent);
  }

  /* Power button */
  .power-btn { color: var(--ctp-overlay1); }
  .power-btn:hover { color: var(--ctp-red); background: color-mix(in srgb, var(--ctp-red) 12%, transparent); }

  /* ── Workspace dots ─────────────────────────────────────────────────── */
  .workspace-dots {
    display:   flex;
    flex-direction: column;
    align-items: center;
    gap:       4px;
    padding:   4px 0;
  }

  .dot {
    width:         6px;
    height:        6px;
    border-radius: 50%;
    background:    var(--ctp-overlay0);
    transition:
      background  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      transform   var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .dot.active {
    background: var(--ctp-mauve);
    transform:  scale(1.3);
  }

  /* ── Workspace label (rotated vertical text) ────────────────────────── */
  .workspace-label {
    writing-mode: vertical-rl;
    text-orientation: mixed;
    transform:    rotate(180deg);
    font-size:    var(--font-size-label);
    font-weight:  var(--font-weight-medium);
    color:        var(--ctp-overlay1);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    user-select:  none;
    white-space:  nowrap;
  }

  /* ── Clock ──────────────────────────────────────────────────────────── */
  .clock {
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            0;
    padding:        4px 0;
    user-select:    none;
    cursor:         default;
  }

  .clock-hour, .clock-min {
    font-size:    var(--font-size-label-sm);
    font-weight:  var(--font-weight-semibold);
    color:        var(--ctp-subtext0);
    line-height:  1.2;
    font-variant-numeric: tabular-nums;
  }

  .clock-sep {
    font-size: 8px;
    color:     var(--ctp-overlay0);
    line-height: 0.8;
  }
</style>
