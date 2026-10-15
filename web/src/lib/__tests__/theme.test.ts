import { describe, it, expect } from 'vitest'
import { ALL_THEMES, resolveTheme, themeFamily, themeMode, themeWithMode } from '../theme'

describe('themeFamily', () => {
  it('maps the default pair to "default"', () => {
    expect(themeFamily('light')).toBe('default')
    expect(themeFamily('dark')).toBe('default')
  })

  it('strips the mode suffix from colored themes', () => {
    expect(themeFamily('rose-light')).toBe('rose')
    expect(themeFamily('blue-dark')).toBe('blue')
  })
})

describe('themeMode', () => {
  it('detects light and dark variants', () => {
    expect(themeMode('light')).toBe('light')
    expect(themeMode('dark')).toBe('dark')
    expect(themeMode('green-light')).toBe('light')
    expect(themeMode('green-dark')).toBe('dark')
  })
})

describe('themeWithMode', () => {
  it('swaps the default pair', () => {
    expect(themeWithMode('light', 'dark')).toBe('dark')
    expect(themeWithMode('dark', 'light')).toBe('light')
  })

  it('keeps the color family', () => {
    expect(themeWithMode('rose-light', 'dark')).toBe('rose-dark')
    expect(themeWithMode('orange-dark', 'light')).toBe('orange-light')
    expect(themeWithMode('yellow-dark', 'dark')).toBe('yellow-dark')
  })

  it('always yields a known theme', () => {
    for (const theme of ALL_THEMES) {
      expect(ALL_THEMES).toContain(themeWithMode(theme, 'light'))
      expect(ALL_THEMES).toContain(themeWithMode(theme, 'dark'))
    }
  })
})

describe('resolveTheme', () => {
  it('returns the chosen theme unchanged when not following the system', () => {
    expect(resolveTheme('rose-light', false, true)).toBe('rose-light')
    expect(resolveTheme('dark', false, false)).toBe('dark')
  })

  it('follows a dark system preference', () => {
    expect(resolveTheme('rose-light', true, true)).toBe('rose-dark')
    expect(resolveTheme('blue-dark', true, true)).toBe('blue-dark')
    expect(resolveTheme('light', true, true)).toBe('dark')
  })

  it('follows a light system preference', () => {
    expect(resolveTheme('rose-dark', true, false)).toBe('rose-light')
    expect(resolveTheme('green-light', true, false)).toBe('green-light')
    expect(resolveTheme('dark', true, false)).toBe('light')
  })
})
