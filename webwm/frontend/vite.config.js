import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [
    svelte({
      onwarn(warning, defaultHandler) {
        // @material/web custom elements (md-*) are real interactive elements
        // but Svelte can't inspect web component internals — suppress all
        // a11y false positives that reference md-* tags in the source frame.
        if (warning.code === 'unknown-prop') return;
        if (warning.code === 'missing-declaration') return;
        if (warning.filename?.includes('@material')) return;
        if (warning.code?.startsWith('a11y-') && warning.frame?.includes('<md-')) return;
        defaultHandler(warning);
      },
    }),
  ],
  server: {
    port: 7703,
    strictPort: true,
  },
});
