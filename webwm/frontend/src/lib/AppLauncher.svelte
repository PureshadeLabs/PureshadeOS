<!--
  AppLauncher.svelte — Full-screen launcher overlay.

  Animates in from bottom with M3 emphasized decel easing.
  Clicking an app chip sends a SpawnApp control message.
  Closes on backdrop click or Escape.
-->
<script>
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import '@material/web/textfield/outlined-text-field.js';
  import { channels } from './ws.js';
  import { openWindow } from './windows.js';

  export let open = false;

  const dispatch = createEventDispatcher();

  // Placeholder app shortcuts — replace with real data from lythmsg
  const pinned = [
    { id: 'terminal',    label: 'Terminal',     icon: 'terminal',      elf: '/usr/bin/lysh'         },
    { id: 'files',       label: 'Files',        icon: 'folder_open',   elf: '/usr/bin/lyfiles'      },
    { id: 'settings',    label: 'Settings',     icon: 'settings',      elf: '/usr/bin/lysettings'   },
    { id: 'browser',     label: 'Browser',      icon: 'language',      elf: '/usr/bin/lybrowser'    },
    { id: 'editor',      label: 'Text Editor',  icon: 'edit_document', elf: '/usr/bin/lyeditor'     },
    { id: 'music',       label: 'Music',        icon: 'music_note',    elf: '/usr/bin/lymusic'      },
    { id: 'calculator',  label: 'Calculator',   icon: 'calculate',     elf: '/usr/bin/lycalc'       },
    { id: 'calendar',    label: 'Calendar',     icon: 'calendar_month',elf: '/usr/bin/lycal'        },
    { id: 'system',      label: 'System Info',  icon: 'info',          elf: '/usr/bin/lysysinfo'    },
    { id: 'screenshot',  label: 'Screenshot',   icon: 'screenshot',    elf: '/usr/bin/lyshot'       },
    { id: 'package',     label: 'Packages',     icon: 'package_2',     elf: '/usr/bin/rpkg-gui'     },
    { id: 'display',     label: 'Display',      icon: 'display_settings', elf: '/usr/bin/lydisplay' },
  ];

  let query = '';
  let inputEl;

  $: filtered = query.trim()
    ? pinned.filter(a => a.label.toLowerCase().includes(query.toLowerCase()))
    : pinned;

  function launch(app) {
    // Open locally in the window store immediately (works without bridge).
    openWindow(app.label, app.icon);
    // Also fire over the bridge when it's connected — bridge will exec the ELF.
    channels.control.send({ type: 'spawn_app', elf_path: app.elf });
    close();
  }

  function close() {
    dispatch('close');
  }

  function handleKey(e) {
    if (!open) return;
    if (e.key === 'Escape') close();
  }

  onMount(() => {
    window.addEventListener('keydown', handleKey);
    // Focus search on open
    if (open && inputEl) setTimeout(() => inputEl.focus(), 50);
  });

  onDestroy(() => {
    window.removeEventListener('keydown', handleKey);
  });

  $: if (open && inputEl) setTimeout(() => inputEl.focus(), 60);
</script>

{#if open}
  <!-- Scrim -->
  <div
    class="scrim"
    role="presentation"
    aria-hidden="true"
    on:click={close}
  ></div>

  <!-- Launcher panel -->
  <div
    class="launcher"
    role="dialog"
    aria-modal="true"
    aria-label="App launcher"
  >
    <!-- Search -->
    <div class="search-wrap">
      <div class="search-box">
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
          <button class="clear-btn icon" aria-label="Clear" on:click={() => query = ''}>close</button>
        {/if}
      </div>
    </div>

    <!-- App grid -->
    <div class="grid-wrap">
      <div class="grid" role="list" aria-label="Applications">
        {#each filtered as app (app.id)}
          <button
            class="app-tile"
            title={app.label}
            aria-label="Launch {app.label}"
            on:click={() => launch(app)}
          >
            <div class="tile-icon-wrap">
              <span
                class="icon tile-icon"
                aria-hidden="true"
              >{app.icon}</span>
            </div>
            <span class="tile-label">{app.label}</span>
          </button>
        {/each}

        {#if filtered.length === 0}
          <div class="empty">
            <span class="icon" style="font-size:32px;color:var(--md-sys-color-outline)">search_off</span>
            <span>No results for "{query}"</span>
          </div>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  /* Scrim */
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.55);
    z-index: 50;
    backdrop-filter: blur(4px);
    animation: scrim-in var(--md-sys-motion-duration-medium2) var(--md-sys-motion-easing-standard) forwards;
  }

  @keyframes scrim-in {
    from { opacity: 0; }
    to   { opacity: 1; }
  }

  /* Launcher panel — centered modal card */
  .launcher {
    position: fixed;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width:  min(680px, calc(100vw - 64px));
    max-height: min(600px, calc(100vh - 120px));
    display: flex;
    flex-direction: column;
    background: var(--md-sys-color-surface-container);
    border-radius: var(--md-sys-shape-corner-extra-large);
    box-shadow: var(--md-sys-elevation-5);
    z-index: 60;
    overflow: hidden;
    animation: launcher-in var(--md-sys-motion-duration-medium3) var(--md-sys-motion-easing-emphasized-decel) forwards;
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  @keyframes launcher-in {
    from { opacity: 0; transform: translate(-50%, calc(-50% + 24px)) scale(0.97); }
    to   { opacity: 1; transform: translate(-50%, -50%)               scale(1);    }
  }

  /* Search */
  .search-wrap {
    padding: 20px 20px 12px;
    flex-shrink: 0;
  }

  .search-box {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--md-sys-color-surface-container-high);
    border-radius: var(--md-sys-shape-corner-full);
    padding: 0 16px;
    height: 48px;
    border: 1px solid var(--md-sys-color-outline-variant);
    transition: border-color var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .search-box:focus-within {
    border-color: var(--md-sys-color-primary);
    outline: none;
  }

  .search-icon {
    color: var(--md-sys-color-outline);
    font-size: 20px;
    flex-shrink: 0;
    font-variation-settings: 'FILL' 0, 'wght' 400, 'GRAD' 0, 'opsz' 20;
  }

  .search-input {
    flex: 1;
    border: none;
    background: transparent;
    color: var(--md-sys-color-on-surface);
    font-size: var(--font-size-body-lg);
    font-family: var(--font-family);
    outline: none;
    min-width: 0;
  }

  .search-input::placeholder {
    color: var(--md-sys-color-outline);
  }

  /* Remove browser search cancel button */
  .search-input::-webkit-search-cancel-button { display: none; }

  .clear-btn {
    background: none;
    border: none;
    cursor: pointer;
    color: var(--md-sys-color-outline);
    font-size: 18px;
    padding: 4px;
    border-radius: var(--md-sys-shape-corner-full);
    line-height: 1;
  }

  .clear-btn:hover { color: var(--md-sys-color-on-surface); }

  /* Grid */
  .grid-wrap {
    flex: 1;
    overflow-y: auto;
    padding: 4px 12px 20px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(100px, 1fr));
    gap: 4px;
  }

  /* App tile */
  .app-tile {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 16px 8px 12px;
    border: none;
    background: transparent;
    color: var(--md-sys-color-on-surface);
    border-radius: var(--md-sys-shape-corner-medium);
    cursor: pointer;
    font-family: var(--font-family);
    transition:
      background var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard),
      transform  var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .app-tile:hover {
    background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
  }

  .app-tile:active {
    background: color-mix(in srgb, var(--md-sys-color-on-surface) 12%, transparent);
    transform: scale(0.95);
  }

  .tile-icon-wrap {
    width:  52px;
    height: 52px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(
      in srgb,
      var(--md-sys-color-primary-container) 70%,
      var(--md-sys-color-surface-container-high) 30%
    );
    border-radius: var(--md-sys-shape-corner-large);
    flex-shrink: 0;
  }

  .tile-icon {
    font-size: 26px;
    color: var(--md-sys-color-on-primary-container);
    font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 24;
  }

  .tile-label {
    font-size: var(--font-size-label-lg);
    font-weight: var(--font-weight-medium);
    text-align: center;
    line-height: 1.2;
    max-width: 90px;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    color: var(--md-sys-color-on-surface-variant);
  }

  .empty {
    grid-column: 1 / -1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 40px 0;
    color: var(--md-sys-color-outline);
    font-size: var(--font-size-body);
  }
</style>
