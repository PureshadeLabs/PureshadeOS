<!--
  App.svelte — root shell.

  Layout:
    layer 0 — wallpaper (CSS gradient or image URL)
    layer 1 — DwindleLayout (tiled windows, wallpaper visible through gaps)
    layer 2 — Taskbar (frosted glass, pinned to bottom)
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import DwindleLayout from './lib/DwindleLayout.svelte';
  import Taskbar       from './lib/Taskbar.svelte';
  import { applyThemeFromWallpaper, applyThemeFromSeed } from './lib/theme.js';
  import { channels }      from './lib/ws.js';
  import { visibleWindows, openWindow } from './lib/windows.js';

  // Apply dark palette before first render — prevents white flash.
  applyThemeFromSeed('#1a0a2e');

  // ── Wallpaper ─────────────────────────────────────────────────────────────
  const wallpaperUrl            = null; // set to image URL when available
  const FALLBACK_WALLPAPER_SEED = '#1a0a2e';

  // ── Bridge: sync window list from control channel ─────────────────────────
  // When the bridge sends an app_list, reconcile it with the local store.
  // Until bridge is running this is a no-op — local state drives the UI.
  const unsub = channels.control.message.subscribe((msg) => {
    if (!msg) return;
    if (msg.type === 'app_spawned') {
      openWindow(msg.name ?? 'App', msg.icon ?? 'apps');
    }
  });

  onMount(() => {
    if (wallpaperUrl) {
      const img       = new Image();
      img.crossOrigin = 'anonymous';
      img.src         = wallpaperUrl;
      img.onload      = () => applyThemeFromWallpaper(img);
      img.onerror     = () => applyThemeFromSeed(FALLBACK_WALLPAPER_SEED);
    } else {
      applyThemeFromSeed(FALLBACK_WALLPAPER_SEED);
    }
  });

  onDestroy(() => unsub());
</script>

<div class="shell">
  <!-- Wallpaper layer -->
  <div
    class="wallpaper"
    role="presentation"
    aria-hidden="true"
    style={wallpaperUrl ? `background-image: url('${wallpaperUrl}')` : ''}
  ></div>

  <!-- Workspace -->
  <div class="workspace">
    <DwindleLayout windows={$visibleWindows} />
  </div>

  <!-- Taskbar -->
  <Taskbar />
</div>

<style>
  .shell {
    width:   100%;
    height:  100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    position: relative;
  }

  .wallpaper {
    position: absolute;
    inset: 0;
    z-index: 0;
    background:
      radial-gradient(ellipse 80% 60% at 20% 80%, rgba(103,58,183,0.25) 0%, transparent 60%),
      radial-gradient(ellipse 60% 50% at 80% 20%, rgba(81,45,168,0.20)  0%, transparent 55%),
      radial-gradient(ellipse 100% 80% at 50% 50%, #130821 0%, #0a0514 100%);
    background-size: cover;
    background-position: center;
    background-repeat: no-repeat;
  }

  .workspace {
    position: relative;
    z-index: 1;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
</style>
