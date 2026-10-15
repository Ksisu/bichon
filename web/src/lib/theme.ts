//
// Copyright (c) 2025-2026 rustmailer.com (https://rustmailer.com)
//
// This file is part of the Bichon Email Archiving Project
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

export const ALL_THEMES = [
  'light', 'dark',
  'rose-light', 'rose-dark',
  'orange-light', 'orange-dark',
  'green-light', 'green-dark',
  'yellow-light', 'yellow-dark',
  'blue-light', 'blue-dark',
] as const

export type Theme = (typeof ALL_THEMES)[number]

export type ThemeMode = 'light' | 'dark'

/** Color family of a theme: 'light'/'dark' → 'default', 'rose-light' → 'rose'. */
export function themeFamily(theme: Theme): string {
  if (theme === 'light' || theme === 'dark') return 'default'
  return theme.slice(0, theme.lastIndexOf('-'))
}

export function themeMode(theme: Theme): ThemeMode {
  return theme === 'dark' || theme.endsWith('-dark') ? 'dark' : 'light'
}

/** The light or dark variant of the same color family. */
export function themeWithMode(theme: Theme, mode: ThemeMode): Theme {
  const family = themeFamily(theme)
  return (family === 'default' ? mode : `${family}-${mode}`) as Theme
}

/**
 * The theme to apply: the user's choice as-is, or — when following the
 * system — its family in the light/dark variant the client prefers.
 */
export function resolveTheme(theme: Theme, followSystem: boolean, prefersDark: boolean): Theme {
  if (!followSystem) return theme
  return themeWithMode(theme, prefersDark ? 'dark' : 'light')
}
