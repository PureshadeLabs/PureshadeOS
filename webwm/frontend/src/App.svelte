<!--
  App.svelte — root shell.

  Layout (row):
    Sidebar (44 px) | Workspace container (flex-1, relative)
                         ├─ wallpaper
                         ├─ DwindleLayout
                         ├─ OSD panel (absolute, top, centered)
                         ├─ AppLauncher (absolute, bottom-center)
                         └─ Notifications (fixed, top-right)
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import DwindleLayout  from './lib/DwindleLayout.svelte';
  import Sidebar        from './lib/Sidebar.svelte';
  import OSD            from './lib/OSD.svelte';
  import AppLauncher    from './lib/AppLauncher.svelte';
  import Notifications  from './lib/Notifications.svelte';
  import { applyThemeFromWallpaper, applyThemeFromSeed } from './lib/theme.js';
  import { channels }      from './lib/ws.js';
  import { visibleWindows, openWindow } from './lib/windows.js';

  applyThemeFromSeed('#1e1e2e');

  const wallpaperUrl            = null;
  const FALLBACK_WALLPAPER_SEED = '#1e1e2e';

  let osdOpen      = false;
  let launcherOpen = false;
  let notifRef;

  const unsub = channels.control.message.subscribe((msg) => {
    if (!msg) return;
    if (msg.type === 'app_spawned') {
      openWindow(msg.name ?? 'App', msg.icon ?? 'apps');
      notifRef?.push({
        title: msg.name ?? 'App',
        body:  'App launched successfully.',
        icon:  msg.icon ?? 'apps',
      });
    }
  });

  onMount(() => {
    if (wallpaperUrl) {
      const img = new Image();
      img.crossOrigin = 'anonymous';
      img.src = wallpaperUrl;
      img.onload  = () => applyThemeFromWallpaper(img);
      img.onerror = () => applyThemeFromSeed(FALLBACK_WALLPAPER_SEED);
    } else {
      applyThemeFromSeed(FALLBACK_WALLPAPER_SEED);
    }
  });

  onDestroy(() => unsub());
</script>

<div class="shell">

  <!-- Left sidebar -->
  <Sidebar
    {osdOpen}
    {launcherOpen}
    on:toggleOSD={()      => { osdOpen = !osdOpen; if (osdOpen) launcherOpen = false; }}
    on:toggleLauncher={() => { launcherOpen = !launcherOpen; if (launcherOpen) osdOpen = false; }}
  />

  <!-- Workspace area (contains wallpaper + windows + floating panels) -->
  <div class="workspace-area">

    <!-- Wallpaper -->
    <div
      class="wallpaper"
      role="presentation"
      aria-hidden="true"
      style={wallpaperUrl ? `background-image: url('${wallpaperUrl}')` : ''}
    ></div>

    <!-- Tiled windows -->
    <div class="workspace">
      <DwindleLayout windows={$visibleWindows} />
    </div>

    <!-- OSD panel -->
    <OSD
      open={osdOpen}
      on:close={() => osdOpen = false}
    />

    <!-- App launcher -->
    <AppLauncher
      open={launcherOpen}
      on:close={() => launcherOpen = false}
    />

    <!-- Notification toasts -->
    <Notifications bind:this={notifRef} />

  </div>

</div>

<style>
  /*
    Border/fillet technique (mirrors Caelestia BlobInvertedRect):
    - Shell background (crust) IS the border — no CSS borders needed
    - workspace-area has margin on all sides except left (sidebar fills left)
    - workspace-area border-radius creates the concave fillet at sidebar junction:
      the crust bg shows through the curved corner gap between sidebar and workspace
  */

  .shell {
    width:          100%;
    height:         100%;
    display:        flex;
    flex-direction: row;
    overflow:       hidden;
    background:     var(--ctp-crust);
  }

  .workspace-area {
    flex:         1;
    position:     relative;
    overflow:     hidden;
    min-width:    0;
    /* 8px crust gap on top/right/bottom = the visible "border" */
    margin:       8px 8px 8px 0;
    /* 20px radius on all corners; left corners create the sidebar fillet */
    border-radius: 20px;
    /* 1px edge line for contrast against the crust border strip */
    box-shadow: inset 0 0 0 1px var(--ctp-surface1);
  }

  .wallpaper {
    position:   absolute;
    inset:      0;
    z-index:    0;
    background: var(--ctp-base);
  }

  .workspace {
    position: relative;
    z-index:  1;
    width:    100%;
    height:   100%;
    overflow: hidden;
  }
</style>
