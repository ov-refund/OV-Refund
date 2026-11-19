// Light / dark theme: light by default; dark only when the user chooses it with the toggle.
import { L } from './i18n';

export const SUN = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>';
export const MOON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></svg>';

function storedTheme(): 'light' | 'dark' | null {
  try { const t = localStorage.getItem('theme'); return t === 'light' || t === 'dark' ? t : null; } catch { return null; }
}
export function isDark(): boolean {
  const t = storedTheme();
  return t === 'dark'; // light is the standard; dark only when chosen
}
export function setTheme(t: 'light' | 'dark') {
  document.documentElement.dataset.theme = t;
  try { localStorage.setItem('theme', t); } catch { /* storage unavailable */ }
}
export function toggleTheme() { setTheme(isDark() ? 'light' : 'dark'); }
/** Button for the bottom-right box (data-theme-toggle). */
export const themeButton = () => {
  const label = isDark() ? L('Licht thema', 'Light theme') : L('Donker thema', 'Dark theme');
  return `<button class="fb theme" data-theme-toggle title="${label}" aria-label="${label}">${isDark() ? SUN : MOON}</button>`;
};
{ const t = storedTheme(); if (t) document.documentElement.dataset.theme = t; }
