/**
 * ws.js — WebSocket channel manager.
 *
 * Creates and maintains three channels:
 *   render  ws://localhost:7700
 *   input   ws://localhost:7701
 *   control ws://localhost:7702
 *
 * Each channel is a Svelte-compatible readable store:
 *   { status: 'connecting'|'open'|'closed'|'error', send(msg) }
 *
 * Channels reconnect automatically on close/error with exponential backoff.
 */

import { writable } from 'svelte/store';

const CHANNELS = {
  render:  'ws://localhost:7700',
  input:   'ws://localhost:7701',
  control: 'ws://localhost:7702',
};

const MAX_BACKOFF_MS = 30_000;

function createChannel(url) {
  const status  = writable('connecting');
  const message = writable(null);
  let ws        = null;
  let backoff   = 500;
  let timer     = null;

  function connect() {
    status.set('connecting');
    ws = new WebSocket(url);

    ws.onopen = () => {
      status.set('open');
      backoff = 500;
    };

    ws.onmessage = (ev) => {
      try { message.set(JSON.parse(ev.data)); }
      catch { message.set(ev.data); }
    };

    ws.onclose = () => {
      status.set('closed');
      schedule();
    };

    ws.onerror = () => {
      status.set('error');
      ws.close();
    };
  }

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => { connect(); }, backoff);
    backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
  }

  function send(msg) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(typeof msg === 'string' ? msg : JSON.stringify(msg));
    }
  }

  connect();

  return { status, message, send };
}

export const channels = {
  render:  createChannel(CHANNELS.render),
  input:   createChannel(CHANNELS.input),
  control: createChannel(CHANNELS.control),
};
