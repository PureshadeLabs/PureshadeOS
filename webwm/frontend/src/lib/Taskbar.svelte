<!--
  Taskbar.svelte — Caelestia-style floating top bar.

  Three pill segments anchored to the top edge:
    Left   — launcher icon + running app chips
    Center — clock / date
    Right  — connection status

  Each pill is a frosted-glass capsule with a strong backdrop blur.
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import '@material/web/chips/chip-set.js';
  import '@material/web/chips/filter-chip.js';

  import { channels }         from './ws.js';
  import ConnectionStatus      from './ConnectionStatus.svelte';
  import AppLauncher            from './AppLauncher.svelte';
  import { windows, focusWindow, minimizeWindow } from './windows.js';

  let time         = formatTime();
  let date         = formatDate();
  let launcherOpen = false;
  let clockTimer;

  $: taskbarApps = $windows;

  function formatTime() {
    return new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  function formatDate() {
    return new Date().toLocaleDateString([], {
      weekday: 'short', month: 'short', day: 'numeric',
    });
  }

  onMount(() => {
    clockTimer = setInterval(() => {
      time = formatTime();
      date = formatDate();
    }, 10_000);
  });

  onDestroy(() => clearInterval(clockTimer));
</script>

<nav class="bar" aria-label="Top bar">

  <!-- Left pill: launcher + running apps -->
  <div class="pill pill-left">
    <button
      class="launcher-btn"
      class:active={launcherOpen}
      title="App Launcher"
      aria-label="Open app launcher"
      aria-expanded={launcherOpen}
      on:click={() => launcherOpen = !launcherOpen}
    >
      <span class="icon" style="font-size:18px; font-variation-settings:'FILL' 1,'wght' 500,'GRAD' 0,'opsz' 20">
        {launcherOpen ? 'close' : 'grid_view'}
      </span>
    </button>

    {#if taskbarApps.length > 0}
      <div class="pill-sep" aria-hidden="true"></div>

      <div class="app-chips" role="list" aria-label="Running apps">
        <md-chip-set>
          {#each taskbarApps as app (app.id)}
            <md-filter-chip
              role="listitem"
              label={app.title}
              selected={app.focused && !app.minimized || undefined}
              on:click={() => app.minimized ? focusWindow(app.id) : minimizeWindow(app.id)}
              aria-label="{app.minimized ? 'Restore' : 'Minimize'} {app.title}"
            >
              <span slot="icon" class="icon chip-icon">{app.icon ?? 'apps'}</span>
            </md-filter-chip>
          {/each}
        </md-chip-set>
      </div>
    {/if}
  </div>

  <!-- Center pill: time -->
  <div class="pill pill-center" role="region" aria-label="Clock">
    <time class="clock-time" datetime={time}>{time}</time>
    <span class="clock-sep" aria-hidden="true">·</span>
    <span class="clock-date">{date}</span>
  </div>

  <!-- Right pill: status -->
  <div class="pill pill-right" role="region" aria-label="System status">
    <ConnectionStatus />
  </div>

</nav>

<AppLauncher open={launcherOpen} on:close={() => launcherOpen = false} />

<style>
  /* ── Bar container ─────────────────────────────────────────────────────── */
  .bar {
    position:  absolute;
    top:       10px;
    left:      12px;
    right:     12px;
    z-index:   100;
    display:   flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    pointer-events: none; /* let click-through to wallpaper gaps */
  }

  /* ── Shared pill style ─────────────────────────────────────────────────── */
  .pill {
    display:         flex;
    align-items:     center;
    gap:             4px;
    height:          38px;
    padding:         0 10px;
    border-radius:   var(--md-sys-shape-corner-full);
    pointer-events:  all;

    /* Glassmorphism */
    background: color-mix(in srgb, var(--ctp-mantle) 82%, transparent);
    backdrop-filter:         blur(28px) saturate(160%);
    -webkit-backdrop-filter: blur(28px) saturate(160%);

    border: 1px solid color-mix(in srgb, var(--ctp-surface2) 60%, transparent);
    box-shadow:
      0 2px 12px rgba(0,0,0,0.45),
      inset 0 1px 0 color-mix(in srgb, white 4%, transparent);
  }

  /* ── Left pill ─────────────────────────────────────────────────────────── */
  .pill-left {
    flex: 1;
    min-width: 0;
    max-width: 480px;
    gap: 6px;
    overflow: hidden;
  }

  /* Launcher button */
  .launcher-btn {
    display:         flex;
    align-items:     center;
    justify-content: center;
    width:           30px;
    height:          30px;
    border:          none;
    border-radius:   var(--md-sys-shape-corner-full);
    background:      color-mix(in srgb, var(--ctp-mauve) 15%, transparent);
    color:           var(--ctp-mauve);
    cursor:          pointer;
    flex-shrink:     0;
    transition:
      background var(--md-sys-motion-duration-short3) var(--md-sys-motion-easing-standard),
      color      var(--md-sys-motion-duration-short3) var(--md-sys-motion-easing-standard);
  }

  .launcher-btn:hover,
  .launcher-btn.active {
    background: color-mix(in srgb, var(--ctp-mauve) 28%, transparent);
    color:      var(--ctp-mauve);
  }

  .launcher-btn.active {
    background: color-mix(in srgb, var(--ctp-mauve) 35%, transparent);
  }

  .pill-sep {
    width:       1px;
    height:      18px;
    background:  var(--ctp-surface2);
    flex-shrink: 0;
    border-radius: 1px;
  }

  .app-chips {
    flex:      1;
    min-width: 0;
    overflow:  hidden;
    display:   flex;
    align-items: center;
  }

  md-chip-set {
    --md-chip-set-gap: 4px;
    display:      flex;
    flex-wrap:    nowrap;
    overflow:     hidden;
  }

  md-filter-chip {
    --md-filter-chip-container-height: 28px;
    --md-filter-chip-label-text-size:  var(--font-size-label);
    --md-filter-chip-outline-color:    transparent;
  }

  .chip-icon {
    font-size: 14px;
    font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 20;
  }

  /* ── Center pill ───────────────────────────────────────────────────────── */
  .pill-center {
    flex-shrink: 0;
    gap: 6px;
    cursor: default;
    user-select: none;
    padding: 0 16px;
  }

  .clock-time {
    font-size:     var(--font-size-title-sm);
    font-weight:   var(--font-weight-semibold);
    color:         var(--ctp-text);
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }

  .clock-sep {
    color:     var(--ctp-overlay1);
    font-size: var(--font-size-body);
  }

  .clock-date {
    font-size:  var(--font-size-label);
    color:      var(--ctp-subtext0);
    white-space: nowrap;
  }

  /* ── Right pill ────────────────────────────────────────────────────────── */
  .pill-right {
    flex-shrink: 0;
  }
</style>
