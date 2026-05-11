<!--
  Notifications.svelte — top-right toast system.

  Accepts an array of notification objects:
    { id, title, body, icon, ts }

  Auto-dismisses after 6 s. Can be dismissed manually.
  Usage: <Notifications bind:notifs />
-->
<script>
  import { onDestroy } from 'svelte';

  export let notifs = [];

  const timers = new Map();

  function dismiss(id) {
    notifs = notifs.filter(n => n.id !== id);
    clearTimeout(timers.get(id));
    timers.delete(id);
  }

  export function push(n) {
    const id = Date.now() + Math.random();
    notifs = [...notifs, { ...n, id, ts: new Date() }];
    timers.set(id, setTimeout(() => dismiss(id), 6000));
    return id;
  }

  function fmtTs(d) { return d.toLocaleTimeString([], { hour:'2-digit', minute:'2-digit' }); }

  onDestroy(() => { for (const t of timers.values()) clearTimeout(t); });
</script>

<div class="notif-container" aria-live="polite" aria-label="Notifications">
  {#each notifs as n (n.id)}
    <div class="toast" role="alert">
      <div class="toast-head">
        <span class="icon toast-icon" aria-hidden="true"
          style="font-variation-settings:'FILL' 1,'wght' 400,'GRAD' 0,'opsz' 20">
          {n.icon ?? 'info'}
        </span>
        <span class="toast-title">{n.title}</span>
        <span class="toast-time">{fmtTs(n.ts)}</span>
        <button class="toast-close" aria-label="Dismiss notification" on:click={() => dismiss(n.id)}>
          <span class="icon" style="font-size:14px">close</span>
        </button>
      </div>
      {#if n.body}
        <p class="toast-body">{n.body}</p>
      {/if}
    </div>
  {/each}
</div>

<style>
  .notif-container {
    position:       fixed;
    top:            14px;
    right:          14px;
    z-index:        200;
    display:        flex;
    flex-direction: column;
    gap:            8px;
    max-width:      320px;
    pointer-events: none;
  }

  .toast {
    pointer-events:  all;
    background:      color-mix(in srgb, var(--ctp-mantle) 92%, transparent);
    backdrop-filter: blur(20px) saturate(140%);
    -webkit-backdrop-filter: blur(20px) saturate(140%);
    border:          1px solid color-mix(in srgb, var(--ctp-surface2) 65%, transparent);
    border-radius:   var(--md-sys-shape-corner-large);
    padding:         10px 12px;
    box-shadow:      0 6px 24px rgba(0,0,0,0.40);
    animation:       toast-in var(--md-sys-motion-duration-medium2) var(--md-sys-motion-easing-emphasized-decel) forwards;
  }

  @keyframes toast-in {
    from { opacity: 0; transform: translateX(20px); }
    to   { opacity: 1; transform: translateX(0);    }
  }

  .toast-head {
    display:     flex;
    align-items: center;
    gap:         6px;
  }

  .toast-icon {
    font-size:   16px;
    color:       var(--ctp-blue);
    flex-shrink: 0;
  }

  .toast-title {
    flex:        1;
    font-size:   var(--font-size-label-lg);
    font-weight: var(--font-weight-semibold);
    color:       var(--ctp-text);
    white-space: nowrap;
    overflow:    hidden;
    text-overflow: ellipsis;
  }

  .toast-time {
    font-size:   var(--font-size-label-sm);
    color:       var(--ctp-overlay1);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .toast-close {
    display:         flex;
    align-items:     center;
    justify-content: center;
    background:      transparent;
    border:          none;
    color:           var(--ctp-overlay1);
    cursor:          pointer;
    padding:         2px;
    border-radius:   var(--md-sys-shape-corner-full);
    flex-shrink:     0;
    transition:      color var(--md-sys-motion-duration-short1) var(--md-sys-motion-easing-standard);
  }

  .toast-close:hover { color: var(--ctp-text); }

  .toast-body {
    font-size:     var(--font-size-body-sm);
    color:         var(--ctp-subtext0);
    margin-top:    4px;
    padding-left:  22px;
    overflow:      hidden;
    display:       -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    line-height:   1.4;
  }
</style>
