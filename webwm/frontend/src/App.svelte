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
  .shell {
    width:     100%;
    height:    100%;
    display:   flex;
    flex-direction: row;
    overflow:  hidden;
    position:  relative;
    background: var(--ctp-crust);
  }

  /* Workspace area fills remaining space */
  .workspace-area {
    flex:     1;
    position: relative;
    overflow: hidden;
    min-width: 0;
  }

  /* Wallpaper */
  .wallpaper {
    position: absolute;
    inset:    0;
    z-index:  0;
    background:
      radial-gradient(ellipse 70% 55% at 15% 85%, rgba(203,166,247,0.10) 0%, transparent 60%),
      radial-gradient(ellipse 55% 45% at 85% 15%, rgba(180,190,254,0.08) 0%, transparent 55%),
      radial-gradient(ellipse 80% 60% at 50% 50%, rgba(137,180,250,0.05) 0%, transparent 70%),
      linear-gradient(145deg, #181825 0%, #11111b 50%, #1e1e2e 100%);
    background-size:     cover;
    background-position: center;
    background-repeat:   no-repeat;
  }

  /* Tiled windows layer */
  .workspace {
    position: relative;
    z-index:  1;
    width:    100%;
    height:   100%;
    overflow: hidden;
  }
</style>
