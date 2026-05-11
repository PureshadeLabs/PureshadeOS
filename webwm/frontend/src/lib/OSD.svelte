<!--
  OSD.svelte — Caelestia central dashboard panel.

  Four tabs mirroring the video:
    Dashboard   — calendar · system info · mini media
    Media       — full media player with sunburst art
    Performance — circular arc gauges (CPU/GPU/Memory/Storage)
    Workspaces  — open windows & workspace switcher

  Triggered by the sidebar logo button.
  Closes on backdrop click or Escape.
-->
<script>
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import { backOut, quintIn } from 'svelte/easing';
  import { windows } from './windows.js';

  function osdIn(node) {
    return {
      duration: 500,
      easing: backOut,
      css: (t, u) => `
        opacity: ${t};
        transform: translateX(-50%) translateY(${-20 * u}px) scaleY(${1 - 0.04 * u});
        transform-origin: top center;
      `
    };
  }

  function osdOut(node) {
    return {
      duration: 220,
      easing: quintIn,
      css: (t, u) => `
        opacity: ${t};
        transform: translateX(-50%) translateY(${-10 * u}px);
      `
    };
  }

  const dispatch = createEventDispatcher();

  export let open = false;

  // ── Tab state ──────────────────────────────────────────────────────────
  const TABS = [
    { id: 'dashboard',   label: 'Dashboard',   icon: 'grid_view'    },
    { id: 'media',       label: 'Media',       icon: 'queue_music'  },
    { id: 'performance', label: 'Performance', icon: 'speed'        },
    { id: 'workspaces',  label: 'Workspaces',  icon: 'workspaces'   },
  ];
  let activeTab = 'dashboard';

  // ── Calendar ───────────────────────────────────────────────────────────
  const now     = new Date();
  const today   = now.getDate();
  const month   = now.toLocaleString('default', { month: 'long' });
  const year    = now.getFullYear();
  // day-of-week headers
  const DOW = ['Mon','Tue','Wed','Thu','Fri','Sat','Sun'];

  function buildCalendar() {
    const first = new Date(year, now.getMonth(), 1);
    // Monday=0 offset
    let startDow = (first.getDay() + 6) % 7;
    const daysInMonth = new Date(year, now.getMonth() + 1, 0).getDate();
    const daysInPrev  = new Date(year, now.getMonth(), 0).getDate();
    const cells = [];
    for (let i = startDow - 1; i >= 0; i--) cells.push({ day: daysInPrev - i, other: true });
    for (let d = 1; d <= daysInMonth; d++) cells.push({ day: d, other: false });
    const remainder = 7 - (cells.length % 7);
    if (remainder < 7) for (let d = 1; d <= remainder; d++) cells.push({ day: d, other: true });
    return cells;
  }

  const calCells = buildCalendar();

  // ── Clock (for dashboard display) ─────────────────────────────────────
  let displayHour = now.getHours();
  let displayMin  = String(now.getMinutes()).padStart(2, '0');
  let displayDate = now.toLocaleDateString('default', { weekday:'short', day:'numeric' });

  // ── Media player stub ─────────────────────────────────────────────────
  const tracks = [
    { title: 'Welcome to RaptorOS', artist: 'WebWM', album: 'System Sounds', duration: 212 },
    { title: 'Kernel Dreams',       artist: 'lythd',  album: 'Boot Sequence', duration: 187 },
    { title: 'Capability Flow',     artist: 'OROS',   album: 'Userspace',     duration: 243 },
  ];
  let trackIdx    = 0;
  let playing     = false;
  let progress    = 0;
  let progressTimer;

  $: track = tracks[trackIdx];
  $: progressPct = track ? (progress / track.duration) * 100 : 0;

  function fmtTime(s) {
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2,'0')}`;
  }

  function togglePlay() {
    playing = !playing;
    if (playing) {
      progressTimer = setInterval(() => {
        progress++;
        if (progress >= track.duration) { progress = 0; nextTrack(); }
      }, 1000);
    } else {
      clearInterval(progressTimer);
    }
  }

  function prevTrack() {
    progress  = 0;
    trackIdx  = (trackIdx - 1 + tracks.length) % tracks.length;
  }

  function nextTrack() {
    progress  = 0;
    trackIdx  = (trackIdx + 1) % tracks.length;
  }

  // ── Performance gauges (simulated) ───────────────────────────────────
  // CPU/GPU/Memory simulated values that animate slowly
  let perf = { cpuTemp: 41, cpuUse: 4, gpuTemp: 54, gpuUse: 6, memGib: 5.4, storGib: 229 };
  let perfTimer;

  function jitter(val, range, min, max) {
    return Math.max(min, Math.min(max, val + (Math.random() - 0.5) * range));
  }

  function tickPerf() {
    perf = {
      cpuTemp: Math.round(jitter(perf.cpuTemp, 2, 30, 95)),
      cpuUse:  Math.round(jitter(perf.cpuUse, 3, 1, 100)),
      gpuTemp: Math.round(jitter(perf.gpuTemp, 2, 30, 95)),
      gpuUse:  Math.round(jitter(perf.gpuUse, 3, 1, 100)),
      memGib:  +jitter(perf.memGib, 0.3, 0.5, 32).toFixed(1),
      storGib: perf.storGib,
    };
  }

  // SVG arc helpers (r=44, stroke-width=8 → circumference ≈ 276)
  const R   = 44;
  const C   = 2 * Math.PI * R; // ≈ 276.5
  function arc(pct) { return C - (C * Math.max(0, Math.min(100, pct)) / 100); }

  // ── Lifecycle ──────────────────────────────────────────────────────────
  function handleKey(e) {
    if (open && e.key === 'Escape') dispatch('close');
  }

  onMount(() => {
    window.addEventListener('keydown', handleKey);
    perfTimer = setInterval(tickPerf, 2500);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKey);
    clearInterval(progressTimer);
    clearInterval(perfTimer);
  });
</script>

{#if open}
  <!-- Backdrop — click to close -->
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div class="backdrop" aria-hidden="true" on:click={() => dispatch('close')}></div>

  <!-- OSD wrapper: handles position + transition + fillets -->
  <div class="osd-wrap" in:osdIn out:osdOut>

    <!-- OSD Panel -->
    <div class="osd" role="dialog" aria-modal="true" aria-label="Dashboard">

    <!-- Tab bar -->
    <nav class="tab-bar" role="tablist" aria-label="Dashboard sections">
      {#each TABS as tab}
        <button
          class="tab"
          class:active={activeTab === tab.id}
          role="tab"
          aria-selected={activeTab === tab.id}
          aria-controls="panel-{tab.id}"
          on:click={() => activeTab = tab.id}
        >
          <span class="icon tab-icon" aria-hidden="true"
            style="font-variation-settings:'FILL' {activeTab===tab.id?1:0},'wght' 400,'GRAD' 0,'opsz' 20">
            {tab.icon}
          </span>
          <span class="tab-label">{tab.label}</span>
        </button>
      {/each}
    </nav>

    <div class="tab-divider"></div>

    <!-- ── DASHBOARD ──────────────────────────────────────────────── -->
    {#if activeTab === 'dashboard'}
      <div class="panel panel-dashboard" id="panel-dashboard" role="tabpanel">

        <!-- Weather stub -->
        <div class="card weather-card">
          <span class="icon weather-icon" aria-hidden="true">wb_sunny</span>
          <div class="weather-info">
            <span class="weather-temp">–°C</span>
            <span class="weather-cond">No data</span>
          </div>
        </div>

        <!-- Calendar -->
        <div class="card cal-card">
          <div class="cal-header">
            <span class="cal-month">{month} {year}</span>
          </div>
          <div class="cal-grid" role="grid">
            {#each DOW as d}
              <span class="cal-dow" role="columnheader">{d}</span>
            {/each}
            {#each calCells as cell}
              <span
                class="cal-day"
                class:today={!cell.other && cell.day === today}
                class:other={cell.other}
                role="gridcell"
                aria-current={!cell.other && cell.day === today ? 'date' : undefined}
              >{cell.day}</span>
            {/each}
          </div>
          <!-- Large day/time display below grid -->
          <div class="cal-time">
            <span class="cal-big-h">{String(displayHour).padStart(2,'0')}</span>
            <span class="cal-dots">•••</span>
            <span class="cal-big-m">{displayMin}</span>
          </div>
          <span class="cal-date-label">{displayDate}</span>
        </div>

        <!-- System info -->
        <div class="card sys-card">
          <div class="sys-row">
            <span class="icon sys-icon" aria-hidden="true">diamond</span>
            <span class="sys-label">RaptorOS</span>
          </div>
          <div class="sys-row">
            <span class="icon sys-icon" aria-hidden="true">window</span>
            <span class="sys-label">WebWM</span>
          </div>
          <div class="sys-row">
            <span class="icon sys-icon" aria-hidden="true">schedule</span>
            <span class="sys-label">up: just booted</span>
          </div>
        </div>

        <!-- Mini media -->
        <div class="card mini-media-card">
          <div class="mini-art">
            <span class="icon" style="font-size:28px;color:var(--ctp-mauve)">
              {playing ? 'graphic_eq' : 'music_note'}
            </span>
          </div>
          <div class="mini-info">
            <span class="mini-title">{track.title}</span>
            <span class="mini-artist">{track.artist}</span>
          </div>
          <div class="mini-controls">
            <button class="ctrl-btn" aria-label="Previous" on:click={prevTrack}>
              <span class="icon" style="font-size:16px">skip_previous</span>
            </button>
            <button class="ctrl-btn play-btn" aria-label={playing?'Pause':'Play'} on:click={togglePlay}>
              <span class="icon" style="font-size:18px">{playing?'pause':'play_arrow'}</span>
            </button>
            <button class="ctrl-btn" aria-label="Next" on:click={nextTrack}>
              <span class="icon" style="font-size:16px">skip_next</span>
            </button>
          </div>
        </div>

      </div>

    <!-- ── MEDIA ───────────────────────────────────────────────────── -->
    {:else if activeTab === 'media'}
      <div class="panel panel-media" id="panel-media" role="tabpanel">

        <!-- Vinyl record album art -->
        <div class="vinyl-wrap" aria-hidden="true">
          <svg class="vinyl-record" class:spinning={playing} viewBox="0 0 140 140">
            <circle cx="70" cy="70" r="69" fill="#111118"/>
            {#each [65,59,53,47,41,35,29] as gr}
              <circle cx="70" cy="70" r="{gr}" fill="none" stroke="rgba(255,255,255,0.045)" stroke-width="0.7"/>
            {/each}
            <circle cx="70" cy="70" r="27" fill="var(--ctp-mauve)"/>
            <circle cx="70" cy="70" r="22" fill="color-mix(in srgb, var(--ctp-mauve) 65%, #000)"/>
            <circle cx="70" cy="70" r="3.5" fill="#111118"/>
          </svg>
        </div>

        <!-- Track info + controls -->
        <div class="media-info">
          <p class="media-title">{track.title}</p>
          <p class="media-album">{track.album}</p>
          <p class="media-artist">{track.artist}</p>

          <div class="media-controls">
            <button class="ctrl-btn" aria-label="Previous" on:click={prevTrack}>
              <span class="icon" style="font-size:20px">skip_previous</span>
            </button>
            <button class="ctrl-btn play-btn large" aria-label={playing?'Pause':'Play'} on:click={togglePlay}>
              <span class="icon" style="font-size:26px">{playing?'pause':'play_arrow'}</span>
            </button>
            <button class="ctrl-btn" aria-label="Next" on:click={nextTrack}>
              <span class="icon" style="font-size:20px">skip_next</span>
            </button>
          </div>

          <!-- Progress bar -->
          <div class="progress-wrap">
            <span class="progress-time">{fmtTime(progress)}</span>
            <div class="progress-bar" role="progressbar"
                 aria-valuenow={progress} aria-valuemax={track.duration}>
              <div class="progress-fill" style="width:{progressPct}%"></div>
            </div>
            <span class="progress-time">{fmtTime(track.duration)}</span>
          </div>

          <!-- Source badge -->
          <div class="source-badge">
            <span class="icon" style="font-size:14px;color:var(--ctp-mauve)">music_note</span>
            <span>WebWM Audio</span>
          </div>
        </div>


      </div>

    <!-- ── PERFORMANCE ─────────────────────────────────────────────── -->
    {:else if activeTab === 'performance'}
      <div class="panel panel-perf" id="panel-performance" role="tabpanel">

        <!-- GPU -->
        <div class="gauge-wrap">
          <svg class="gauge-svg" viewBox="0 0 100 100" aria-hidden="true">
            <circle cx="50" cy="50" r="{R}" fill="none"
                    stroke="var(--ctp-surface0)" stroke-width="8"/>
            <circle cx="50" cy="50" r="{R}" fill="none"
                    stroke="var(--ctp-blue)" stroke-width="8"
                    stroke-dasharray="{C}" stroke-dashoffset="{arc(perf.gpuUse)}"
                    stroke-linecap="round" transform="rotate(-90 50 50)"/>
          </svg>
          <div class="gauge-label">
            <span class="gauge-value">{perf.gpuTemp}°C</span>
            <span class="gauge-sub">GPU temp</span>
            <span class="gauge-pct">{perf.gpuUse}% Usage</span>
          </div>
        </div>

        <!-- CPU -->
        <div class="gauge-wrap">
          <svg class="gauge-svg" viewBox="0 0 100 100" aria-hidden="true">
            <circle cx="50" cy="50" r="{R}" fill="none"
                    stroke="var(--ctp-surface0)" stroke-width="8"/>
            <circle cx="50" cy="50" r="{R}" fill="none"
                    stroke="var(--ctp-mauve)" stroke-width="8"
                    stroke-dasharray="{C}" stroke-dashoffset="{arc(perf.cpuUse)}"
                    stroke-linecap="round" transform="rotate(-90 50 50)"/>
          </svg>
          <div class="gauge-label">
            <span class="gauge-value">{perf.cpuTemp}°C</span>
            <span class="gauge-sub">CPU temp</span>
            <span class="gauge-pct">{perf.cpuUse}% Usage</span>
          </div>
        </div>

        <!-- Memory / Storage -->
        <div class="gauge-wrap">
          <svg class="gauge-svg" viewBox="0 0 100 100" aria-hidden="true">
            <circle cx="50" cy="50" r="{R}" fill="none"
                    stroke="var(--ctp-surface0)" stroke-width="8"/>
            <circle cx="50" cy="50" r="{R}" fill="none"
                    stroke="var(--ctp-peach)" stroke-width="8"
                    stroke-dasharray="{C}" stroke-dashoffset="{arc((perf.memGib/32)*100)}"
                    stroke-linecap="round" transform="rotate(-90 50 50)"/>
          </svg>
          <div class="gauge-label">
            <span class="gauge-value">{perf.memGib} GiB</span>
            <span class="gauge-sub">Memory</span>
            <span class="gauge-pct">{perf.storGib} GiB Storage</span>
          </div>
        </div>

      </div>

    <!-- ── WORKSPACES ──────────────────────────────────────────────── -->
    {:else if activeTab === 'workspaces'}
      <div class="panel panel-ws" id="panel-workspaces" role="tabpanel">

        <div class="ws-header">
          <span class="ws-name">Desktop</span>
          <span class="ws-count">{$windows.length} window{$windows.length !== 1 ? 's' : ''}</span>
        </div>

        {#if $windows.length === 0}
          <div class="ws-empty">
            <span class="icon" style="font-size:32px;color:var(--ctp-surface2)">dashboard</span>
            <span>No open windows</span>
          </div>
        {:else}
          <div class="ws-window-list">
            {#each $windows as win}
              <div class="ws-window-row">
                <span class="icon ws-win-icon">{win.icon ?? 'apps'}</span>
                <span class="ws-win-title">{win.title}</span>
                <span class="ws-win-state">{win.minimized ? 'minimized' : win.focused ? 'focused' : 'open'}</span>
              </div>
            {/each}
          </div>
        {/if}

      </div>
    {/if}

  </div><!-- .osd -->

  </div><!-- .osd-wrap -->
{/if}

<style>
  /* ── Backdrop ────────────────────────────────────────────────────────── */
  .backdrop {
    position:        fixed;
    inset:           0;
    z-index:         29;
    background:      transparent;
  }

  /* ── OSD wrapper — positioning, transitions, fillets live here ────────── */
  .osd-wrap {
    position:  absolute;
    top:       0;
    left:      50%;
    transform: translateX(-50%);
    width:     min(700px, calc(100% - 32px));
    z-index:   30;
  }


  /* ── OSD Panel ───────────────────────────────────────────────────────── */
  .osd {
    display:        flex;
    flex-direction: column;
    border-radius:  0 0 20px 20px;
    overflow:       hidden;

    background:  var(--ctp-crust);
    border:      1px solid var(--ctp-surface0);
    border-top:  none;
    box-shadow:  0 20px 60px rgba(0,0,0,0.60);
  }

  /* ── Tab bar ─────────────────────────────────────────────────────────── */
  .tab-bar {
    display:   flex;
    padding:   0 8px;
    flex-shrink: 0;
  }

  .tab {
    flex:           1;
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            3px;
    padding:        12px 4px 14px;
    border:         none;
    background:     transparent;
    color:          var(--ctp-overlay1);
    cursor:         pointer;
    font-family:    var(--font-family);
    position:       relative;
    transition:
      color var(--md-sys-motion-duration-short3) var(--md-sys-motion-easing-standard);
  }

  .tab:hover { color: var(--ctp-subtext1); }

  .tab.active {
    color: var(--ctp-mauve);
  }

  .tab.active::before {
    content:       '';
    position:      absolute;
    inset:         5px 10px 11px;
    background:    color-mix(in srgb, var(--ctp-mauve) 18%, transparent);
    border-radius: var(--md-sys-shape-corner-full);
    border:        1px solid color-mix(in srgb, var(--ctp-mauve) 24%, transparent);
  }

  .tab-icon { font-size: 18px; }
  .tab-label { font-size: var(--font-size-label); font-weight: var(--font-weight-medium); }

  .tab-divider {
    height:     1px;
    background: color-mix(in srgb, var(--ctp-surface2) 60%, transparent);
    flex-shrink: 0;
  }

  /* ── Shared panel base ───────────────────────────────────────────────── */
  .panel {
    padding:   16px;
    flex:      1;
    display:   flex;
    gap:       12px;
  }

  /* ── DASHBOARD ───────────────────────────────────────────────────────── */
  .panel-dashboard {
    display:   grid;
    grid-template-columns: auto 1fr auto auto;
    grid-template-rows: auto auto;
    gap:       10px;
    padding:   14px;
    align-items: start;
  }

  .card {
    background:    color-mix(in srgb, var(--ctp-surface0) 35%, var(--ctp-crust));
    border-radius: var(--md-sys-shape-corner-large);
    border:        1px solid color-mix(in srgb, var(--ctp-surface2) 55%, transparent);
    padding:       12px;
  }

  /* Weather */
  .weather-card {
    display:     flex;
    align-items: center;
    gap:         10px;
    grid-column: 1;
    grid-row:    1;
    white-space: nowrap;
  }

  .weather-icon {
    font-size: 28px;
    color:     var(--ctp-yellow);
    font-variation-settings: 'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 24;
  }

  .weather-temp {
    font-size:   var(--font-size-title-lg);
    font-weight: var(--font-weight-semibold);
    color:       var(--ctp-text);
    display:     block;
    line-height: 1;
  }

  .weather-cond {
    font-size: var(--font-size-label);
    color:     var(--ctp-subtext0);
  }

  /* Calendar */
  .cal-card {
    grid-column: 2;
    grid-row:    1 / 3;
  }

  .cal-header {
    display:     flex;
    align-items: center;
    margin-bottom: 8px;
  }

  .cal-month {
    font-size:   var(--font-size-label-lg);
    font-weight: var(--font-weight-semibold);
    color:       var(--ctp-text);
  }

  .cal-grid {
    display:               grid;
    grid-template-columns: repeat(7, 1fr);
    gap:                   2px;
    margin-bottom:         8px;
  }

  .cal-dow {
    font-size:   var(--font-size-label-sm);
    color:       var(--ctp-overlay1);
    text-align:  center;
    padding:     2px 0;
    font-weight: var(--font-weight-medium);
  }

  .cal-day {
    font-size:   var(--font-size-label-sm);
    color:       var(--ctp-subtext1);
    text-align:  center;
    padding:     3px 2px;
    border-radius: var(--md-sys-shape-corner-full);
    line-height: 1;
    cursor:      default;
    font-variant-numeric: tabular-nums;
    transition:  background var(--md-sys-motion-duration-short1) var(--md-sys-motion-easing-standard);
  }

  .cal-day.today {
    background: var(--ctp-mauve);
    color:      var(--ctp-crust);
    font-weight: var(--font-weight-bold);
  }

  .cal-day.other { color: var(--ctp-overlay0); }

  .cal-time {
    display:     flex;
    align-items: center;
    gap:         6px;
    margin-top:  6px;
  }

  .cal-big-h, .cal-big-m {
    font-size:    var(--font-size-display);
    font-weight:  var(--font-weight-bold);
    color:        var(--ctp-text);
    line-height:  1;
    font-variant-numeric: tabular-nums;
  }

  .cal-dots {
    font-size: var(--font-size-label-sm);
    color:     var(--ctp-overlay1);
    letter-spacing: -1px;
    transform: translateY(-4px);
  }

  .cal-date-label {
    font-size:   var(--font-size-label);
    color:       var(--ctp-subtext0);
    margin-top:  2px;
    display:     block;
  }

  /* System info */
  .sys-card {
    grid-column: 3;
    grid-row:    1;
    display:     flex;
    flex-direction: column;
    gap:         8px;
    white-space: nowrap;
  }

  .sys-row {
    display:     flex;
    align-items: center;
    gap:         8px;
  }

  .sys-icon {
    font-size: 16px;
    color:     var(--ctp-blue);
    font-variation-settings: 'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 20;
  }

  .sys-label {
    font-size:   var(--font-size-label-lg);
    color:       var(--ctp-subtext1);
  }

  /* Mini media */
  .mini-media-card {
    grid-column: 3;
    grid-row:    2;
    display:     flex;
    flex-direction: column;
    align-items: center;
    gap:         8px;
    text-align:  center;
  }

  .mini-art {
    width:           48px;
    height:          48px;
    border-radius:   50%;
    background:      color-mix(in srgb, var(--ctp-mauve) 15%, var(--ctp-surface0));
    display:         flex;
    align-items:     center;
    justify-content: center;
  }

  .mini-title {
    font-size:   var(--font-size-label-lg);
    font-weight: var(--font-weight-semibold);
    color:       var(--ctp-text);
    max-width:   120px;
    overflow:    hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mini-artist {
    font-size: var(--font-size-label);
    color:     var(--ctp-subtext0);
  }

  .mini-controls, .media-controls {
    display:     flex;
    align-items: center;
    gap:         4px;
  }

  /* ── MEDIA ───────────────────────────────────────────────────────────── */
  .panel-media {
    align-items: center;
    padding:     20px 24px;
    gap:         24px;
  }

  /* Vinyl record */
  .vinyl-wrap {
    flex-shrink: 0;
    width:       140px;
    height:      140px;
  }

  .vinyl-record {
    width:         100%;
    height:        100%;
    display:       block;
    border-radius: 50%;
    box-shadow:
      0 8px 32px rgba(0,0,0,0.65),
      inset 0 0 0 1px rgba(255,255,255,0.04);
  }

  @keyframes vinyl-spin { to { transform: rotate(360deg); } }

  .vinyl-record.spinning {
    animation: vinyl-spin 2.5s linear infinite;
  }

  /* Track info */
  .media-info {
    flex:           1;
    display:        flex;
    flex-direction: column;
    gap:            8px;
    min-width:      0;
  }

  .media-title {
    font-size:   var(--font-size-title-lg);
    font-weight: var(--font-weight-bold);
    color:       var(--ctp-text);
    white-space: nowrap;
    overflow:    hidden;
    text-overflow: ellipsis;
  }

  .media-album {
    font-size:   var(--font-size-label-lg);
    color:       var(--ctp-subtext0);
    white-space: nowrap;
  }

  .media-artist {
    font-size:   var(--font-size-label-lg);
    color:       var(--ctp-overlay1);
  }

  /* Controls */
  .ctrl-btn {
    display:         flex;
    align-items:     center;
    justify-content: center;
    background:      transparent;
    border:          none;
    color:           var(--ctp-subtext0);
    cursor:          pointer;
    border-radius:   var(--md-sys-shape-corner-full);
    width:           36px;
    height:          36px;
    transition:
      color       var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      background  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .ctrl-btn:hover {
    background: color-mix(in srgb, var(--ctp-surface1) 80%, transparent);
    color:      var(--ctp-text);
  }

  .play-btn {
    background: color-mix(in srgb, var(--ctp-mauve) 20%, var(--ctp-surface0));
    color:      var(--ctp-mauve);
    width:      44px;
    height:     44px;
  }

  .play-btn:hover {
    background: color-mix(in srgb, var(--ctp-mauve) 32%, var(--ctp-surface0));
    color:      var(--ctp-mauve);
  }

  .play-btn.large { width: 52px; height: 52px; }

  /* Progress */
  .progress-wrap {
    display:     flex;
    align-items: center;
    gap:         8px;
    margin-top:  4px;
  }

  .progress-time {
    font-size:  var(--font-size-label-sm);
    color:      var(--ctp-overlay1);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .progress-bar {
    flex:           1;
    height:         4px;
    background:     var(--ctp-surface1);
    border-radius:  2px;
    overflow:       hidden;
  }

  .progress-fill {
    height:         100%;
    background:     var(--ctp-mauve);
    border-radius:  2px;
    transition:     width 1s linear;
  }

  /* Source badge */
  .source-badge {
    display:         inline-flex;
    align-items:     center;
    gap:             5px;
    padding:         3px 10px;
    background:      color-mix(in srgb, var(--ctp-mauve) 15%, var(--ctp-surface0));
    border:          1px solid color-mix(in srgb, var(--ctp-mauve) 25%, transparent);
    border-radius:   var(--md-sys-shape-corner-full);
    font-size:       var(--font-size-label);
    color:           var(--ctp-mauve);
    align-self:      flex-start;
    margin-top:      2px;
  }

  /* ── PERFORMANCE ─────────────────────────────────────────────────────── */
  .panel-perf {
    justify-content: center;
    align-items:     center;
    padding:         24px 32px;
  }

  .gauge-wrap {
    flex:            1;
    display:         flex;
    flex-direction:  column;
    align-items:     center;
    gap:             10px;
    position:        relative;
  }

  .gauge-svg {
    width:  160px;
    height: 160px;
    /* Smooth arc transitions */
    --gauge-transition: stroke-dashoffset var(--md-sys-motion-duration-medium4) var(--md-sys-motion-easing-standard);
  }

  .gauge-svg circle:last-child {
    transition: var(--gauge-transition);
  }

  .gauge-label {
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            2px;
    text-align:     center;
  }

  .gauge-value {
    font-size:   var(--font-size-title-lg);
    font-weight: var(--font-weight-bold);
    color:       var(--ctp-text);
    line-height: 1;
  }

  .gauge-sub {
    font-size:   var(--font-size-label);
    color:       var(--ctp-subtext0);
  }

  .gauge-pct {
    font-size:   var(--font-size-label-sm);
    color:       var(--ctp-overlay1);
  }

  /* ── WORKSPACES ──────────────────────────────────────────────────────── */
  .panel-ws {
    flex-direction: column;
    gap:            12px;
  }

  .ws-header {
    display:     flex;
    align-items: baseline;
    gap:         10px;
  }

  .ws-name {
    font-size:   var(--font-size-title);
    font-weight: var(--font-weight-semibold);
    color:       var(--ctp-text);
  }

  .ws-count {
    font-size: var(--font-size-label);
    color:     var(--ctp-subtext0);
  }

  .ws-empty {
    flex:           1;
    display:        flex;
    flex-direction: column;
    align-items:    center;
    justify-content: center;
    gap:            10px;
    color:          var(--ctp-overlay1);
    font-size:      var(--font-size-body);
  }

  .ws-window-list {
    display:        flex;
    flex-direction: column;
    gap:            6px;
  }

  .ws-window-row {
    display:     flex;
    align-items: center;
    gap:         10px;
    padding:     8px 10px;
    background:  color-mix(in srgb, var(--ctp-surface0) 50%, transparent);
    border-radius: var(--md-sys-shape-corner-medium);
    border:      1px solid color-mix(in srgb, var(--ctp-surface2) 40%, transparent);
  }

  .ws-win-icon {
    font-size: 18px;
    color:     var(--ctp-mauve);
    font-variation-settings: 'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 20;
  }

  .ws-win-title {
    flex:      1;
    font-size: var(--font-size-body);
    color:     var(--ctp-text);
    overflow:  hidden;
    text-overflow: ellipsis;
    white-space:   nowrap;
  }

  .ws-win-state {
    font-size:   var(--font-size-label-sm);
    color:       var(--ctp-overlay1);
    white-space: nowrap;
  }
</style>
