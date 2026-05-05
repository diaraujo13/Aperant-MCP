/**
 * App language tracking module for main process.
 *
 * Tracks the user's in-app language setting (not OS locale) for use in
 * main process code that needs localized strings (e.g., context menus).
 *
 * Updated via IPC when user changes language in settings.
 */

import { app } from 'electron';

// Current app language, defaults to 'en'
// Updated via setAppLanguage() when renderer notifies of language change
let currentAppLanguage = 'en';

function normalizeLanguage(language: string | undefined): string {
  if (!language) {
    return 'en';
  }

  const normalized = language.toLowerCase();
  if (normalized === 'pt-br' || normalized === 'pt_br' || normalized.startsWith('pt-') || normalized === 'pt') {
    return 'pt-BR';
  }

  if (normalized.startsWith('fr')) {
    return 'fr';
  }

  return 'en';
}

/**
 * Get the current app language.
 * Falls back to 'en' if not set.
 */
export function getAppLanguage(): string {
  return currentAppLanguage;
}

/**
 * Set the current app language.
 * Called by IPC handler when renderer changes language.
 */
export function setAppLanguage(language: string): void {
  currentAppLanguage = normalizeLanguage(language);
}

/**
 * Initialize app language from OS locale as a starting point.
 * The renderer will update this once i18n initializes.
 */
export function initAppLanguage(): void {
  try {
    // app.getLocale() may not be available in test environments
    const osLocale = app?.getLocale?.() || 'en';
    currentAppLanguage = normalizeLanguage(osLocale);
  } catch {
    currentAppLanguage = 'en';
  }
}
