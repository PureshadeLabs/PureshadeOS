<!--
  App.svelte — root shell.

  Layout:
    layer 0 — wallpaper (Caelestia-style Catppuccin gradient)
    layer 1 — DwindleLayout (tiled windows, offset for top bar)
    layer 2 — Taskbar (three floating pills, absolutely positioned at top)
-->
<script>
  import { onMount, onDestroy } from 'svelte';
  import DwindleLayout from './lib/DwindleLayout.svelte';
  import Taskbar       from './lib/Taskbar.svelte';
  import { applyThemeFromWallpaper, applyThemeFromSeed } from './lib/theme.js';
  import { channels }      from './lib/ws.js';
  import { visibleWindows, openWindow } from './lib/windows.js';

  // Catppuccin Mocha seed — deep mauve-black
  applyThemeFromSeed('#1e1e2e');

  const wallpaperUrl            = null;
  const FALLBACK_WALLPAPER_SEED = '#1e1e2e';

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
  <!-- Wallpaper layer — Caelestia-style dark cosmic gradient -->
  <div
    class="wallpaper"
    role="presentation"
    aria-hidden="true"
    style={wallpaperUrl ? `background-image: url('${wallpaperUrl}')` : ''}
  ></div>

  <!-- Workspace — padded top to clear the floating bar -->
  <div class="workspace">
    <DwindleLayout windows={$visibleWindows} />
  </div>

  <!-- Floating top bar (absolutely positioned, z-index 100) -->
  <Taskbar />
</div>

<style>
  .shell {
    width:     100%;
    height:    100%;
    display:   flex;
    flex-direction: column;
    overflow:  hidden;
    position:  relative;
  }

  /* Caelestia-style wallpaper: deep purple-blue Catppuccin gradient */
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

  .workspace {
    position:   relative;
    z-index:    1;
    flex:       1;
    min-height: 0;
    overflow:   hidden;
    /* Inset top padding so tiled windows don't hide under the bar */
    padding-top: 58px;
  }
</style>
