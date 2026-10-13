// The typed doorway to the Rust side. Nothing else in the UI calls `invoke`.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type {
  Badge,
  BtCandidate,
  DevicesSnapshot,
  HealthSummary,
  RideInit,
  RideState,
  RideSummary,
  RouteMeta,
  Settings,
  StravaStatus
} from './types'

/** Subscribes to a backend event; returns a synchronous unsubscribe. */
function on<T>(event: string, cb: (payload: T) => void): () => void {
  const pending = listen<T>(event, (e) => cb(e.payload))
  return () => void pending.then((unlisten) => unlisten())
}

export const api = {
  kv: {
    get: <T>(name: string) => invoke<T | null>('kv_get', { name }),
    set: (name: string, value: unknown) => invoke<void>('kv_set', { name, value }),
    del: (name: string) => invoke<void>('kv_del', { name })
  },
  routes: {
    importDialog: () => invoke<{ added: RouteMeta[]; problems: string[] }>('routes_import_dialog'),
    importText: (name: string, text: string) => invoke<RouteMeta>('routes_import_text', { name, text }),
    demoMeta: () => invoke<RouteMeta>('demo_meta')
  },
  badges: () => invoke<Badge[]>('badges'),
  chooseDir: () => invoke<string | null>('choose_dir'),
  revealFile: (path: string) => invoke<void>('reveal_file', { path }),
  openExternal: (url: string) => invoke<void>('open_external', { url }),
  devices: {
    list: () => invoke<DevicesSnapshot>('devices_list'),
    onChanged: (cb: (s: DevicesSnapshot) => void) => on('devices:changed', cb),
    scanStart: () => invoke<void>('ble_scan_start'),
    scanStop: () => invoke<void>('ble_scan_stop'),
    onCandidates: (cb: (c: BtCandidate[]) => void) => on('ble:candidates', cb),
    connect: (id: string) => invoke<void>('ble_connect', { id }),
    antStart: () => invoke<void>('ant_start'),
    antStop: () => invoke<void>('ant_stop'),
    demoAdd: () => invoke<void>('demo_add'),
    demoNudge: (delta: number) => invoke<void>('demo_nudge', { delta }),
    remove: (id: string) => invoke<void>('device_remove', { id })
  },
  ride: {
    start: (routeId: string) => invoke<RideInit>('ride_start', { routeId }),
    routeData: () => invoke<ArrayBuffer>('ride_route_data'),
    roadMesh: () => invoke<ArrayBuffer>('ride_road_mesh'),
    chunk: (cx: number, cz: number) => invoke<ArrayBuffer>('terrain_chunk', { cx, cz }),
    far: (cx: number, cz: number) => invoke<ArrayBuffer>('terrain_far', { cx, cz }),
    togglePause: () => invoke<void>('ride_toggle_pause'),
    updateSettings: (settings: Settings) => invoke<void>('ride_update_settings', { settings }),
    finish: () => invoke<RideSummary | null>('ride_finish'),
    abort: () => invoke<void>('ride_abort'),
    onState: (cb: (s: RideState) => void) => on('ride:state', cb),
    onLoading: (cb: (message: string) => void) => on('ride:loading', cb)
  },
  strava: {
    status: () => invoke<StravaStatus>('strava_status'),
    setApp: (clientId: string, clientSecret: string) => invoke<StravaStatus>('strava_set_app', { clientId, clientSecret }),
    connect: () => invoke<StravaStatus>('strava_connect'),
    disconnect: () => invoke<StravaStatus>('strava_disconnect'),
    upload: (rideId: string, filePath: string, name: string, description: string) =>
      invoke<number>('strava_upload', { rideId, filePath, name, description })
  },
  health: {
    import: () => invoke<HealthSummary | null>('health_import')
  },
  smoke: {
    config: () => invoke<{ screen: string; ms: number } | null>('smoke_config'),
    done: (png: string | null, info: string) => invoke<void>('smoke_done', { png, info })
  },
  log: (message: string) => invoke<void>('log', { message })
}

/** Commands reject with a plain string; show that rather than "[object Object]". */
export function errorText(err: unknown): string {
  return typeof err === 'string' ? err : err instanceof Error ? err.message : String(err)
}

export const platform: 'win32' | 'darwin' | 'linux' = /Windows/i.test(navigator.userAgent) ? 'win32' : /Mac/i.test(navigator.userAgent) ? 'darwin' : 'linux'
