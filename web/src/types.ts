// Shapes of the data exchanged with the Rust side. The Rust structs in
// `crates/core/src/types.rs` and `src-tauri/src` are the source of truth.

import type { Climb } from './core/gpx'

export interface Settings {
  rider: { name: string; weightKg: number; bikeKg: number; ftp: number; maxHr: number }
  physics: { cda: number; crr: number; maxSpeedKph: number }
  trainer: { difficulty: number; mode: 'sim' | 'erg'; ergWatts: number; virtualPower: boolean }
  units: 'metric' | 'imperial'
  bots: { enabled: boolean; wkg: number[] }
  realTerrain: boolean
  buildings: boolean
  scenery: boolean
  stravaAutoUpload: boolean
  exportDir: string
}

export const DEFAULT_SETTINGS: Settings = {
  rider: { name: 'Rider', weightKg: 75, bikeKg: 9, ftp: 200, maxHr: 185 },
  physics: { cda: 0.32, crr: 0.004, maxSpeedKph: 80 },
  trainer: { difficulty: 0.75, mode: 'sim', ergWatts: 150, virtualPower: false },
  units: 'metric',
  bots: { enabled: true, wkg: [1.5, 2.2, 3.0] },
  realTerrain: true,
  buildings: true,
  scenery: true,
  stravaAutoUpload: false,
  exportDir: ''
}

export interface RouteMeta {
  id: string
  name: string
  distance: number
  ascent: number
  climbs: number
  addedAt: number
  /** Simplified [lat, lon] outline for list previews. */
  preview: [number, number][]
}

export interface RideSummary {
  id: string
  routeId: string
  routeName: string
  startedAt: number
  durationS: number
  distanceM: number
  ascentM: number
  avgPower: number
  maxPower: number
  np: number
  avgHr: number
  maxHr: number
  avgCadence: number
  avgSpeed: number
  kj: number
  tss: number
  completed: boolean
  isPb: boolean
  xp: number
  newBadges: string[]
  filePath: string
  stravaActivityId?: number
}

export interface Profile {
  xp: number
  rides: number
  totalDistance: number
  totalAscent: number
  badges: string[]
}

export const EMPTY_PROFILE: Profile = { xp: 0, rides: 0, totalDistance: 0, totalAscent: 0, badges: [] }

export interface Badge {
  id: string
  name: string
  description: string
}

export interface StravaStatus {
  configured: boolean
  clientId: string
  connected: boolean
  athlete: string
}

export interface HealthSummary {
  weightKg: number | null
  weightDate: string | null
  restingHr: number | null
  maxHrObserved: number | null
  vo2max: number | null
  ftp: number | null
  dateOfBirth: string | null
  cyclingWorkouts: number
  records: number
}

export interface BtCandidate {
  id: string
  name: string
}

export interface Readings {
  power: number | null
  cadence: number | null
  hr: number | null
  wheelSpeed: number | null
}

export interface DeviceInfo {
  id: string
  name: string
  transport: 'ble' | 'ant' | 'sim'
  kind: 'trainer' | 'power' | 'hr' | 'cadence' | 'speed'
  status: 'connecting' | 'connected' | 'searching' | 'disconnected'
  controllable: boolean
  readings: Readings
}

export interface DevicesSnapshot {
  devices: DeviceInfo[]
  ant: { state: 'off' | 'opening' | 'scanning' | 'error'; message: string }
}

export interface WorldLayout {
  /** Vertices across the road ribbon, per route sample. */
  roadSection: number
  chunk: number
  cells: number
  farCells: number
  farCell: number
  viewChunks: number
  roadWidth: number
  roadLift: number
}

export interface RivalInfo {
  id: string
  name: string
  kind: 'ghost' | 'bot'
  color: number
}

/** What the Rust side returns when a ride session starts. */
export interface RideInit {
  name: string
  step: number
  n: number
  total: number
  climbs: Climb[]
  realTerrain: boolean
  /** How many OpenStreetMap buildings were placed along the route. */
  buildings: number
  /** What other OpenStreetMap scenery the route has. */
  scenery: { forests: number; waters: number; bridges: number; tunnels: number; overbridges: number; roads: number; roadBridges: number }
  /** Stretches of the route in a tunnel or under cover, as `[from, to]` in metres. */
  covered: [number, number][]
  layout: WorldLayout
  rivals: RivalInfo[]
  pbClimbs: Record<number, number>
}

export interface ClimbResult {
  index: number
  climb: Climb
  timeS: number
  previousBest: number | null
}

/** Streamed from the simulation about 20 times a second. */
export interface RideState {
  distance: number
  speed: number
  power: number
  hr: number | null
  cadence: number | null
  elapsed: number
  grade: number
  ele: number
  ascent: number
  moving: boolean
  paused: boolean
  finished: boolean
  hasPower: boolean
  noSensors: boolean
  hasTrainer: boolean
  lat: number
  lon: number
  gap: number | null
  climbIndex: number | null
  climbElapsed: number
  climbResult: ClimbResult | null
  rivals: { id: string; distance: number; speed: number }[]
  demoTarget: number | null
  /** Seconds recorded so far; rides under 60 are not saved. */
  samples: number
}
