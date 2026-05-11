<!--
  AppLauncher.svelte — Caelestia-style Spotlight launcher overlay.

  Full-screen blurred backdrop + centered search bar with results below.
  Closes on Escape or backdrop click.
-->
<script>
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import { channels } from './ws.js';
  import { openWindow } from './windows.js';

  export let open = false;

  const dispatch = createEventDispatcher();

  const pinned = [
    { id: 'terminal',    label: 'Terminal',     icon: 'terminal',         elf: '/usr/bin/lysh'         },
    { id: 'files',       label: 'Files',        icon: 'folder_open',      elf: '/usr/bin/lyfiles'      },
    { id: 'settings',    label: 'Settings',     icon: 'settings',         elf: '/usr/bin/lysettings'   },
    { id: 'browser',     label: 'Browser',      icon: 'language',         elf: '/usr/bin/lybrowser'    },
    { id: 'editor',      label: 'Text Editor',  icon: 'edit_document',    elf: '/usr/bin/lyeditor'     },
    { id: 'music',       label: 'Music',        icon: 'music_note',       elf: '/usr/bin/lymusic'      },
    { id: 'calculator',  label: 'Calculator',   icon: 'calculate',        elf: '/usr/bin/lycalc'       },
    { id: 'calendar',    label: 'Calendar',     icon: 'calendar_month',   elf: '/usr/bin/lycal'        },
    { id: 'system',      label: 'System Info',  icon: 'info',             elf: '/usr/bin/lysysinfo'    },
    { id: 'screenshot',  label: 'Screenshot',   icon: 'screenshot',       elf: '/usr/bin/lyshot'       },
    { id: 'package',     label: 'Packages',     icon: 'package_2',        elf: '/usr/bin/rpkg-gui'     },
    { id: 'display',     label: 'Display',      icon: 'display_settings', elf: '/usr/bin/lydisplay'    },
  ];

  let query = '';
  let inputEl;

  $: filtered = query.trim()
    ? pinned.filter(a => a.label.toLowerCase().includes(query.toLowerCase()))
    : pinned;

  function launch(app) {
    openWindow(app.label, app.icon);
    channels.control.send({ type: 'spawn_app', elf_path: app.elf });
    close();
  }

  function close() {
    query = '';
    dispatch('close');
  }

  function handleKey(e) {
    if (!open) return;
    if (e.key === 'Escape') close();
  }

  onMount(() => window.addEventListener('keydown', handleKey));
  onDestroy(() => window.removeEventListener('keydown', handleKey));

  $: if (open && inputEl) setTimeout(() => inputEl.focus(), 60);
</script>

{#if open}
  <!-- Full-screen backdrop -->
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div
    class="backdrop"
    aria-hidden="true"
    on:click={close}
  ></div>

  <!-- Launcher card -->
  <div
    class="launcher"
    role="dialog"
    aria-modal="true"
    aria-label="App launcher"
  >
    <!-- Search bar -->
    <div class="search-row">
      <span class="icon search-icon">search</span>
      <input
        bind:this={inputEl}
        bind:value={query}
        class="search-input"
        type="search"
        placeholder="Search apps…"
        aria-label="Search apps"
        autocomplete="off"
        spellcheck="false"
      />
      {#if query}
        <button class="clear-btn" aria-label="Clear search" on:click={() => query = ''}>
          <span class="icon" style="font-size:16px">close</span>
        </button>
      {/if}
    </div>

    <!-- Results -->
    <div class="grid-wrap">
      {#if filtered.length > 0}
        <div class="grid" role="list" aria-label="Applications">
          {#each filtered as app (app.id)}
            <button
              class="app-tile"
              title={app.label}
              aria-label="Launch {app.label}"
              on:click={() => launch(app)}
            >
              <div class="tile-icon-wrap">
                <span class="icon tile-icon" aria-hidden="true">{app.icon}</span>
              </div>
              <span class="tile-label">{app.label}</span>
            </button>
          {/each}
        </div>
      {:else}
        <div class="empty">
          <span class="icon" style="font-size:28px">search_off</span>
          <span>No results for "{query}"</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  /* Backdrop */
  .backdrop {
    position:        fixed;
    inset:           0;
    z-index:         50;
    background:      rgba(0, 0, 0, 0.60);
    backdrop-filter: blur(12px) saturate(80%);
    -webkit-backdrop-filter: blur(12px) saturate(80%);
    animation: fade-in var(--md-sys-motion-duration-medium1) var(--md-sys-motion-easing-standard) forwards;
  }

  @keyframes fade-in {
    from { opacity: 0; }
    to   { opacity: 1; }
  }

  /* Launcher card */
  .launcher {
    position:  fixed;
    left:      50%;
    top:       50%;
    transform: translate(-50%, -50%);
    width:     min(600px, calc(100vw - 48px));
    max-height: min(540px, calc(100vh - 100px));
    display:   flex;
    flex-direction: column;
    z-index:   60;
    overflow:  hidden;
    border-radius: var(--md-sys-shape-corner-extra-large);

    background: color-mix(in srgb, var(--ctp-mantle) 88%, transparent);
    backdrop-filter:         blur(32px) saturate(150%);
    -webkit-backdrop-filter: blur(32px) saturate(150%);
    border: 1px solid color-mix(in srgb, var(--ctp-surface2) 70%, transparent);
    box-shadow:
      0 24px 60px rgba(0,0,0,0.55),
      inset 0 1px 0 color-mix(in srgb, white 5%, transparent);

    animation: slide-in var(--md-sys-motion-duration-medium2) var(--md-sys-motion-easing-emphasized-decel) forwards;
  }

  @keyframes slide-in {
    from { opacity: 0; transform: translate(-50%, calc(-50% + 20px)) scale(0.97); }
    to   { opacity: 1; transform: translate(-50%, -50%)              scale(1);    }
  }

  /* Search row */
  .search-row {
    display:      flex;
    align-items:  center;
    gap:          10px;
    padding:      16px 20px;
    border-bottom: 1px solid color-mix(in srgb, var(--ctp-surface2) 50%, transparent);
    flex-shrink:  0;
  }

  .search-icon {
    color:       var(--ctp-overlay1);
    font-size:   20px;
    flex-shrink: 0;
    font-variation-settings: 'FILL' 0, 'wght' 400, 'GRAD' 0, 'opsz' 20;
  }

  .search-input {
    flex:       1;
    border:     none;
    background: transparent;
    color:      var(--ctp-text);
    font-size:  var(--font-size-title);
    font-family: var(--font-family);
    font-weight: var(--font-weight-medium);
    outline:    none;
    min-width:  0;
  }

  .search-input::placeholder { color: var(--ctp-overlay0); }
  .search-input::-webkit-search-cancel-button { display: none; }

  .clear-btn {
    display:         flex;
    align-items:     center;
    justify-content: center;
    background:      color-mix(in srgb, var(--ctp-surface2) 60%, transparent);
    border:          none;
    cursor:          pointer;
    color:           var(--ctp-subtext0);
    width:           24px;
    height:          24px;
    border-radius:   var(--md-sys-shape-corner-full);
    padding:         0;
    flex-shrink:     0;
    transition: background var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .clear-btn:hover {
    background: var(--ctp-surface2);
    color:      var(--ctp-text);
  }

  /* Grid */
  .grid-wrap {
    flex:       1;
    overflow-y: auto;
    padding:    16px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(96px, 1fr));
    gap: 6px;
  }

  /* App tile */
  .app-tile {
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            8px;
    padding:        14px 8px 10px;
    border:         none;
    background:     transparent;
    color:          var(--ctp-text);
    border-radius:  var(--md-sys-shape-corner-large);
    cursor:         pointer;
    font-family:    var(--font-family);
    transition:
      background  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      transform   var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .app-tile:hover {
    background: color-mix(in srgb, var(--ctp-surface0) 80%, transparent);
  }

  .app-tile:active {
    background: color-mix(in srgb, var(--ctp-surface1) 80%, transparent);
    transform:  scale(0.94);
  }

  /* Icon bubble */
  .tile-icon-wrap {
    width:           52px;
    height:          52px;
    display:         flex;
    align-items:     center;
    justify-content: center;
    background:      color-mix(in srgb, var(--ctp-mauve) 18%, var(--ctp-surface0));
    border-radius:   var(--md-sys-shape-corner-large);
    flex-shrink:     0;
    border:          1px solid color-mix(in srgb, var(--ctp-mauve) 15%, transparent);
    transition:
      background  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      border-color var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .app-tile:hover .tile-icon-wrap {
    background: color-mix(in srgb, var(--ctp-mauve) 28%, var(--ctp-surface0));
    border-color: color-mix(in srgb, var(--ctp-mauve) 30%, transparent);
  }

  .tile-icon {
    font-size: 24px;
    color:     var(--ctp-mauve);
    font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 24;
  }

  .tile-label {
    font-size:   var(--font-size-label);
    font-weight: var(--font-weight-medium);
    text-align:  center;
    line-height: 1.3;
    max-width:   84px;
    overflow:    hidden;
    display:     -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    color:       var(--ctp-subtext1);
  }

  /* Empty state */
  .empty {
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            12px;
    padding:        48px 0;
    color:          var(--ctp-overlay1);
    font-size:      var(--font-size-body);
  }
</style>
