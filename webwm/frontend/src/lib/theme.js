/**
 * theme.js — M3 dynamic color engine.
 *
 * Usage:
 *   import { applyThemeFromWallpaper, applyThemeFromSeed } from './theme.js';
 *
 *   // From a loaded <img> element (must be CORS-accessible):
 *   await applyThemeFromWallpaper(imgElement);
 *
 *   // From a hex seed directly:
 *   applyThemeFromSeed('#6750a4');
 *
 * Writes --md-sys-color-* custom properties to :root via applyTheme().
 * Because theme.css transitions color/background on *, changes animate.
 */

import {
  argbFromHex,
  themeFromSourceColor,
  applyTheme,
} from '@material/material-color-utilities';

/** Fallback seed — matches the #1a0a2e gradient wallpaper. */
export const FALLBACK_SEED = '#1a0a2e';

/**
 * Extract dominant color from a loaded HTMLImageElement and apply M3 theme.
 * Falls back to FALLBACK_SEED on any error (CORS taint, load failure, etc.).
 */
export async function applyThemeFromWallpaper(imgEl) {
  try {
    const { default: ColorThief } = await import('color-thief-browser');
    const thief = new ColorThief();
    const [r, g, b] = thief.getColor(imgEl);
    const hex = '#'
      + r.toString(16).padStart(2, '0')
      + g.toString(16).padStart(2, '0')
      + b.toString(16).padStart(2, '0');
    applyThemeFromSeed(hex);
  } catch (err) {
    console.warn('[theme] wallpaper extraction failed, using fallback:', err);
    applyThemeFromSeed(FALLBACK_SEED);
  }
}

/**
 * Generate and apply a full M3 dark-scheme palette from a hex seed color.
 */
export function applyThemeFromSeed(hex) {
  try {
    const argb  = argbFromHex(hex);
    const theme = themeFromSourceColor(argb);
    applyTheme(theme, { target: document.documentElement, dark: true });
  } catch (err) {
    console.error('[theme] applyThemeFromSeed failed:', err);
  }
}
