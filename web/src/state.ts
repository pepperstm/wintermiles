import { createContext, useContext, useEffect, useState, useSyncExternalStore } from 'react'
import { api } from './bridge'
import { DEFAULT_SETTINGS, type Badge, type DevicesSnapshot, type Settings } from './types'

export { api }

/** Stored settings layered over the defaults, so new options appear in old profiles. */
export function mergeSettings(stored: Partial<Settings> | null): Settings {
  const s = stored ?? {}
  return {
    ...DEFAULT_SETTINGS,
    ...s,
    rider: { ...DEFAULT_SETTINGS.rider, ...s.rider },
    physics: { ...DEFAULT_SETTINGS.physics, ...s.physics },
    trainer: { ...DEFAULT_SETTINGS.trainer, ...s.trainer },
    bots: { ...DEFAULT_SETTINGS.bots, ...s.bots }
  }
}

export interface SettingsStore {
  settings: Settings
  update(patch: (s: Settings) => Settings): void
}

export const SettingsContext = createContext<SettingsStore>({ settings: DEFAULT_SETTINGS, update: () => undefined })

export function useSettings(): SettingsStore {
  return useContext(SettingsContext)
}

// The device list lives in Rust; this mirrors it and follows its change events.
let devices: DevicesSnapshot = { devices: [], ant: { state: 'off', message: '' } }
const deviceListeners = new Set<() => void>()
const setDevices = (next: DevicesSnapshot): void => {
  devices = next
  deviceListeners.forEach((fn) => fn())
}
api.devices.onChanged(setDevices)
export const refreshDevices = (): Promise<void> => api.devices.list().then(setDevices)
void refreshDevices()

const subscribeDevices = (fn: () => void): (() => void) => {
  deviceListeners.add(fn)
  return () => deviceListeners.delete(fn)
}

/** The connected sensors. Re-renders when devices connect, disconnect or change status. */
export function useDevices(): DevicesSnapshot {
  return useSyncExternalStore(subscribeDevices, () => devices)
}

/** Loads a JSON document from the app's data folder. `undefined` while loading. */
export function useStored<T>(name: string, fallback: T): [T | undefined, (value: T) => void] {
  const [value, setValue] = useState<T | undefined>(undefined)
  useEffect(() => {
    let live = true
    void api.kv.get<T>(name).then((v) => live && setValue(v ?? fallback))
    return () => {
      live = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [name])
  const save = (next: T): void => {
    setValue(next)
    void api.kv.set(name, next)
  }
  return [value, save]
}

let badgeCatalogue: Promise<Badge[]> | null = null

/** The badge catalogue, defined once in the Rust core. */
export function useBadges(): Badge[] {
  const [badges, setBadges] = useState<Badge[]>([])
  useEffect(() => {
    badgeCatalogue ??= api.badges()
    void badgeCatalogue.then(setBadges)
  }, [])
  return badges
}

/** Set by the ride screen while a 3D view exists, so the automated check can grab a frame. */
export const smokeHooks: { capture: (() => string) | null; state: unknown; world: unknown } = { capture: null, state: null, world: null }
