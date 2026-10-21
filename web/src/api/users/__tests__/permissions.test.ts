import { describe, it, expect } from 'vitest'
import { getPermissions } from '../api'

// Returns the key itself, so each label is the i18n key it was built from.
const identity = (key: string) => key

type LocaleTree = { [key: string]: string | LocaleTree }

const locales = import.meta.glob<LocaleTree>('../../../locales/*.json', {
  eager: true,
  import: 'default',
})

function resolve(tree: LocaleTree, key: string): unknown {
  return key
    .split('.')
    .reduce<unknown>(
      (node, part) =>
        node && typeof node === 'object'
          ? (node as LocaleTree)[part]
          : undefined,
      tree
    )
}

describe('getPermissions', () => {
  const permissions = getPermissions(identity, { isEnterprise: true })

  it('offers system:metrics with its own label', () => {
    const entry = permissions.find((p) => p.value === 'system:metrics')
    expect(entry?.label).toBe('permission.system.metrics')
  })

  it('lists every permission once', () => {
    const values = permissions.map((p) => p.value)
    expect(new Set(values).size).toBe(values.length)
  })

  it('has a translation for every label in every locale', () => {
    expect(Object.keys(locales).length).toBeGreaterThan(0)
    for (const [file, tree] of Object.entries(locales)) {
      for (const { label } of permissions) {
        expect(typeof resolve(tree, label), `${file}: ${label}`).toBe('string')
      }
    }
  })
})
