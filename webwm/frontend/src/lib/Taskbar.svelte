<!--
  Taskbar.svelte — M3 bottom panel with frosted-glass effect.

  Sections (left → right):
    Launcher button | App chips (filter chips) | [spacer] | Tray (status + clock)

  Uses @material/web:
    <md-filter-chip>  for running app entries
    <md-icon-button>  for the launcher
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import '@material/web/chips/chip-set.js';
  import '@material/web/chips/filter-chip.js';
  import '@material/web/iconbutton/filled-tonal-icon-button.js';
  import '@material/web/divider/divider.js';

  import { channels }         from './ws.js';
  import ConnectionStatus      from './ConnectionStatus.svelte';
  import AppLauncher            from './AppLauncher.svelte';
  import { windows, focusWindow, minimizeWindow } from './windows.js';

  // ── State ──────────────────────────────────────────────────────────────
  let time         = formatTime();
  let date         = formatDate();
  let launcherOpen = false;
  let clockTimer;

  // All windows (including minimized) shown in taskbar.
  $: taskbarApps = $windows;

  // ── Helpers ─────────────────────────────────────────────────────────────
  function formatTime() {
    return new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  function formatDate() {
    return new Date().toLocaleDateString([], {
      weekday: 'short', month: 'short', day: 'numeric',
    });
  }

  // ── Lifecycle ────────────────────────────────────────────────────────────
  onMount(() => {
    clockTimer = setInterval(() => {
      time = formatTime();
      date = formatDate();
    }, 10_000);
  });

  onDestroy(() => clearInterval(clockTimer));
</script>

<nav class="taskbar" aria-label="Taskbar">

  <!-- Launcher button -->
  <md-filled-tonal-icon-button
    class="launcher"
    title="App Launcher"
    aria-label="Open app launcher"
    aria-expanded={launcherOpen}
    on:click={() => launcherOpen = !launcherOpen}
  >
    <span class="icon" style="font-size:22px; font-variation-settings:'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 24">
      {launcherOpen ? 'close' : 'apps'}
    </span>
  </md-filled-tonal-icon-button>

  <md-divider inset vertical></md-divider>

  <!-- Running apps as filter chips -->
  <div class="app-region" role="list" aria-label="Running apps">
    <md-chip-set class="chip-set">
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

    {#if taskbarApps.length === 0}
      <span class="no-apps">
        <span class="icon" style="font-size:14px;vertical-align:-2px;margin-right:4px">desktop_windows</span>
        No apps running
      </span>
    {/if}
  </div>

  <div class="spacer"></div>

  <!-- System tray -->
  <div class="tray" role="region" aria-label="System tray">
    <ConnectionStatus />

    <md-divider inset vertical></md-divider>

    <!-- Clock -->
    <button class="clock-btn" title="{date}" aria-label="Current time: {time}, {date}">
      <time class="clock-time" datetime={time}>{time}</time>
      <span class="clock-date">{date}</span>
    </button>
  </div>

</nav>

<AppLauncher open={launcherOpen} on:close={() => launcherOpen = false} />

<style>
  .taskbar {
    display: flex;
    align-items: center;
    height: 52px;
    padding: 0 8px;
    gap: 6px;

    /* Frosted glass */
    background: color-mix(
      in srgb,
      var(--md-sys-color-surface-container) 85%,
      transparent
    );
    backdrop-filter: blur(20px) saturate(180%);
    -webkit-backdrop-filter: blur(20px) saturate(180%);

    /* M3 elevation — hairline border on top */
    border-top: 1px solid var(--md-sys-color-outline-variant);
    box-shadow: var(--md-sys-elevation-2);
    position: relative;
    z-index: 10;
  }

  /* Launcher */
  .taskbar .launcher {
    --md-filled-tonal-icon-button-container-width:  40px;
    --md-filled-tonal-icon-button-container-height: 40px;
    --md-filled-tonal-icon-button-container-color:  var(--md-sys-color-secondary-container);
    --md-filled-tonal-icon-button-icon-color:       var(--md-sys-color-on-secondary-container);
    flex-shrink: 0;
  }

  /* Divider sizing */
  md-divider {
    --md-divider-color: var(--md-sys-color-outline-variant);
    height: 28px;
    flex-shrink: 0;
    margin: 0 2px;
  }

  /* App chip area */
  .app-region {
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
    overflow: hidden;
  }

  .chip-set {
    --md-chip-set-gap: 4px;
    display: flex;
    flex-wrap: nowrap;
    overflow: hidden;
  }

  /* Chip overrides */
  md-filter-chip {
    --md-filter-chip-container-height: 32px;
    --md-filter-chip-label-text-size:  var(--font-size-label-lg);
  }

  .chip-icon {
    font-size: 16px;
    font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 20;
  }

  .no-apps {
    font-size: var(--font-size-body-sm);
    color: var(--md-sys-color-outline);
    padding-left: 8px;
    white-space: nowrap;
    display: flex;
    align-items: center;
  }

  .spacer { flex: 1; }

  /* Tray */
  .tray {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  /* Clock */
  .clock-btn {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    justify-content: center;
    padding: 4px 8px;
    border: none;
    background: transparent;
    color: var(--md-sys-color-on-surface);
    cursor: default;
    border-radius: var(--md-sys-shape-corner-small);
    min-width: 64px;
    gap: 1px;
  }

  .clock-btn:hover {
    background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
    cursor: pointer;
  }

  .clock-time {
    font-size: var(--font-size-title-sm);
    font-weight: var(--font-weight-semibold);
    line-height: 1;
    letter-spacing: -0.01em;
    font-variant-numeric: tabular-nums;
  }

  .clock-date {
    font-size: var(--font-size-label);
    color: var(--md-sys-color-on-surface-variant);
    line-height: 1;
    white-space: nowrap;
  }
</style>
