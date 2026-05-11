<!--
  AppLauncher.svelte — Caelestia-style list launcher.

  Floating card at bottom-center.
  App list (icon + name + description) on top.
  Search bar at the BOTTOM (matching the video).
  Closes on Escape or backdrop click.
-->
<script>
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import { channels } from './ws.js';
  import { openWindow } from './windows.js';

  export let open = false;

  const dispatch = createEventDispatcher();

  const apps = [
    { id: 'terminal',   label: 'Terminal',        desc: 'System shell',              icon: 'terminal',         elf: '/usr/bin/lysh'         },
    { id: 'files',      label: 'Files',            desc: 'File manager',              icon: 'folder_open',      elf: '/usr/bin/lyfiles'      },
    { id: 'settings',   label: 'Settings',         desc: 'System preferences',        icon: 'settings',         elf: '/usr/bin/lysettings'   },
    { id: 'browser',    label: 'Browser',          desc: 'Web browser',               icon: 'language',         elf: '/usr/bin/lybrowser'    },
    { id: 'editor',     label: 'Text Editor',      desc: 'Edit documents',            icon: 'edit_document',    elf: '/usr/bin/lyeditor'     },
    { id: 'music',      label: 'Music',            desc: 'Audio player',              icon: 'music_note',       elf: '/usr/bin/lymusic'      },
    { id: 'calculator', label: 'Calculator',       desc: 'Quick calculations',        icon: 'calculate',        elf: '/usr/bin/lycalc'       },
    { id: 'calendar',   label: 'Calendar',         desc: 'Dates and events',          icon: 'calendar_month',   elf: '/usr/bin/lycal'        },
    { id: 'system',     label: 'System Info',      desc: 'Hardware and OS details',   icon: 'info',             elf: '/usr/bin/lysysinfo'    },
    { id: 'screenshot', label: 'Screenshot',       desc: 'Capture the screen',        icon: 'screenshot',       elf: '/usr/bin/lyshot'       },
    { id: 'package',    label: 'Package Manager',  desc: 'Install and update rpkg',   icon: 'package_2',        elf: '/usr/bin/rpkg-gui'     },
    { id: 'display',    label: 'Display Settings', desc: 'Resolution and scaling',    icon: 'display_settings', elf: '/usr/bin/lydisplay'    },
  ];

  let query = '';
  let focusIdx = 0;
  let inputEl;

  $: filtered = query.trim()
    ? apps.filter(a =>
        a.label.toLowerCase().includes(query.toLowerCase()) ||
        a.desc.toLowerCase().includes(query.toLowerCase())
      )
    : apps;

  $: if (filtered) focusIdx = 0;

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
    if (e.key === 'Escape') { close(); return; }
    if (e.key === 'ArrowUp')   { e.preventDefault(); focusIdx = Math.max(0, focusIdx - 1); }
    if (e.key === 'ArrowDown') { e.preventDefault(); focusIdx = Math.min(filtered.length - 1, focusIdx + 1); }
    if (e.key === 'Enter' && filtered[focusIdx]) { launch(filtered[focusIdx]); }
  }

  onMount(() => window.addEventListener('keydown', handleKey));
  onDestroy(() => window.removeEventListener('keydown', handleKey));

  $: if (open && inputEl) setTimeout(() => inputEl.focus(), 50);
</script>

{#if open}
  <!-- Backdrop -->
  <!-- svelte-ignore a11y-click-events-have-key-events a11y-no-noninteractive-element-interactions -->
  <div class="backdrop" aria-hidden="true" on:click={close}></div>

  <!-- Launcher card -->
  <div class="launcher" role="dialog" aria-modal="true" aria-label="Application launcher">

    <!-- App list -->
    <div class="list-wrap">
      {#if filtered.length === 0}
        <div class="empty">
          <span class="icon" style="font-size:24px">search_off</span>
          <span>No results for "{query}"</span>
        </div>
      {:else}
        <ul class="app-list" role="listbox" aria-label="Applications">
          {#each filtered as app, i (app.id)}
            <!-- svelte-ignore a11y-click-events-have-key-events -->
            <li
              class="app-row"
              class:focused={i === focusIdx}
              role="option"
              aria-selected={i === focusIdx}
              on:click={() => launch(app)}
              on:mouseenter={() => focusIdx = i}
            >
              <div class="app-icon-wrap">
                <span class="icon app-icon" aria-hidden="true"
                  style="font-variation-settings:'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 20">
                  {app.icon}
                </span>
              </div>
              <div class="app-text">
                <span class="app-name">{app.label}</span>
                <span class="app-desc">{app.desc}</span>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Divider -->
    <div class="divider" aria-hidden="true"></div>

    <!-- Search bar (bottom) -->
    <div class="search-row">
      <span class="icon search-icon" aria-hidden="true">search</span>
      <input
        bind:this={inputEl}
        bind:value={query}
        class="search-input"
        type="search"
        placeholder='Type ">" for commands'
        aria-label="Search applications"
        autocomplete="off"
        spellcheck="false"
      />
      {#if query}
        <button class="clear-btn" aria-label="Clear" on:click={() => { query = ''; inputEl.focus(); }}>
          <span class="icon" style="font-size:14px">close</span>
        </button>
      {/if}
    </div>

  </div>
{/if}

<style>
  /* Backdrop */
  .backdrop {
    position:   fixed;
    inset:      0;
    z-index:    49;
    background: transparent;
  }

  /* Launcher card */
  .launcher {
    position:       fixed;
    left:           50%;
    bottom:         60px;
    transform:      translateX(-50%);
    width:          min(560px, calc(100vw - 80px));
    max-height:     min(480px, calc(100vh - 120px));
    display:        flex;
    flex-direction: column;
    z-index:        50;
    border-radius:  var(--md-sys-shape-corner-extra-large);
    overflow:       hidden;

    background:      color-mix(in srgb, var(--ctp-mantle) 92%, transparent);
    backdrop-filter: blur(32px) saturate(160%);
    -webkit-backdrop-filter: blur(32px) saturate(160%);
    border:          1px solid color-mix(in srgb, var(--ctp-surface2) 70%, transparent);
    box-shadow:
      0 20px 50px rgba(0,0,0,0.55),
      inset 0 1px 0 color-mix(in srgb, white 5%, transparent);

    animation: launcher-in var(--md-sys-motion-duration-medium2) var(--md-sys-motion-easing-emphasized-decel) forwards;
  }

  @keyframes launcher-in {
    from { opacity: 0; transform: translateX(-50%) translateY(14px); }
    to   { opacity: 1; transform: translateX(-50%) translateY(0);    }
  }

  /* List */
  .list-wrap {
    flex:       1;
    overflow-y: auto;
    padding:    6px;
  }

  .app-list {
    list-style: none;
    display:    flex;
    flex-direction: column;
    gap:        2px;
  }

  /* Row */
  .app-row {
    display:       flex;
    align-items:   center;
    gap:           12px;
    padding:       8px 10px;
    border-radius: var(--md-sys-shape-corner-medium);
    cursor:        pointer;
    transition:    background var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .app-row.focused {
    background: color-mix(in srgb, var(--ctp-mauve) 12%, var(--ctp-surface0));
  }

  .app-row:hover {
    background: color-mix(in srgb, var(--ctp-surface0) 80%, transparent);
  }

  .app-row.focused:hover {
    background: color-mix(in srgb, var(--ctp-mauve) 16%, var(--ctp-surface0));
  }

  /* App icon bubble */
  .app-icon-wrap {
    width:           36px;
    height:          36px;
    border-radius:   var(--md-sys-shape-corner-medium);
    background:      color-mix(in srgb, var(--ctp-surface1) 80%, transparent);
    display:         flex;
    align-items:     center;
    justify-content: center;
    flex-shrink:     0;
    transition:      background var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .app-row.focused .app-icon-wrap {
    background: color-mix(in srgb, var(--ctp-mauve) 20%, var(--ctp-surface0));
  }

  .app-icon {
    font-size: 18px;
    color:     var(--ctp-subtext1);
    transition: color var(--md-sys-motion-duration-short2) var(--md-sys-motion-easing-standard);
  }

  .app-row.focused .app-icon { color: var(--ctp-mauve); }

  /* Text */
  .app-text {
    flex:       1;
    min-width:  0;
    display:    flex;
    flex-direction: column;
    gap:        1px;
  }

  .app-name {
    font-size:   var(--font-size-body);
    font-weight: var(--font-weight-semibold);
    color:       var(--ctp-text);
    white-space: nowrap;
    overflow:    hidden;
    text-overflow: ellipsis;
  }

  .app-desc {
    font-size:   var(--font-size-label-sm);
    color:       var(--ctp-subtext0);
    white-space: nowrap;
    overflow:    hidden;
    text-overflow: ellipsis;
  }

  /* Empty */
  .empty {
    display:        flex;
    flex-direction: column;
    align-items:    center;
    gap:            10px;
    padding:        32px 0;
    color:          var(--ctp-overlay1);
    font-size:      var(--font-size-body);
  }

  /* Divider */
  .divider {
    height:     1px;
    background: color-mix(in srgb, var(--ctp-surface2) 55%, transparent);
    flex-shrink: 0;
  }

  /* Search bar (bottom) */
  .search-row {
    display:      flex;
    align-items:  center;
    gap:          10px;
    padding:      12px 16px;
    flex-shrink:  0;
  }

  .search-icon {
    color:       var(--ctp-overlay1);
    font-size:   18px;
    flex-shrink: 0;
    font-variation-settings: 'FILL' 0,'wght' 400,'GRAD' 0,'opsz' 20;
  }

  .search-input {
    flex:        1;
    border:      none;
    background:  transparent;
    color:       var(--ctp-text);
    font-size:   var(--font-size-body);
    font-family: var(--font-family);
    outline:     none;
    min-width:   0;
  }

  .search-input::placeholder { color: var(--ctp-overlay0); }
  .search-input::-webkit-search-cancel-button { display: none; }

  .clear-btn {
    display:         flex;
    align-items:     center;
    justify-content: center;
    background:      color-mix(in srgb, var(--ctp-surface2) 55%, transparent);
    border:          none;
    cursor:          pointer;
    color:           var(--ctp-subtext0);
    width:           22px;
    height:          22px;
    border-radius:   var(--md-sys-shape-corner-full);
    padding:         0;
    flex-shrink:     0;
    transition:      background var(--md-sys-motion-duration-short1) var(--md-sys-motion-easing-standard);
  }

  .clear-btn:hover { background: var(--ctp-surface2); color: var(--ctp-text); }
</style>
