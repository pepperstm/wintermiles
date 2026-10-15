// Shapes of the data exchanged with the Rust side. The Rust structs in
// `crates/core/src/types.rs` and `src-tauri/src` are the source of truth.

import type { Climb } from './core/gpx'

export interface Settings {
  rider: { name: string; weightKg: number; bikeKg: number; ftp: number; maxHr: number }
  physics: { cda: number; crr: number; maxSpeedKph: number }
  trainer: { difficulty: number; mode: 'sim' | 'erg'; ergWatts: number; virtualPower: boolean; maxGrade: number; downhill: number; workoutErg: boolean }
  units: 'metric' | 'imperial'
  bots: { enabled: boolean; wkg: number[] }
  realTerrain: boolean
  buildings: boolean
  scenery: boolean
  stravaAutoUpload: boolean
  exportDir: string
  goal: { name: string; distanceKm: number; deadline: string; from: string }
  look: { timeOfDay: TimeOfDay; weather: Weather; landmarks: boolean; photos: boolean; mapillaryToken: string }
  smart: { kind: 'none' | 'homeassistant' | 'shelly' | 'tasmota' | 'webhook'; url: string; fanEntity: string; lightEntity: string; fanBy: 'speed' | 'power' | 'hr'; fanFrom: number; fanFull: number }
  group: { relayUrl: string }
}

export type TimeOfDay = 'dawn' | 'day' | 'evening' | 'dusk' | 'night' | 'clock'
export type Weather = 'clear' | 'cloudy' | 'rain' | 'fog' | 'snow' | 'live'

export const DEFAULT_SETTINGS: Settings = {
  rider: { name: 'Rider', weightKg: 75, bikeKg: 9, ftp: 200, maxHr: 185 },
  physics: { cda: 0.32, crr: 0.004, maxSpeedKph: 80 },
  trainer: { difficulty: 0.75, mode: 'sim', ergWatts: 150, virtualPower: false, maxGrade: 0.15, downhill: 0.5, workoutErg: true },
  units: 'metric',
  bots: { enabled: true, wkg: [1.5, 2.2, 3.0] },
  realTerrain: true,
  buildings: true,
  scenery: true,
  stravaAutoUpload: false,
  exportDir: '',
  goal: { name: 'Winter miles', distanceKm: 1000, deadline: '', from: '' },
  look: { timeOfDay: 'day', weather: 'clear', landmarks: true, photos: false, mapillaryToken: '' },
  smart: { kind: 'none', url: '', fanEntity: '', lightEntity: '', fanBy: 'speed', fanFrom: 10, fanFull: 35 },
  group: { relayUrl: 'ws://127.0.0.1:47800' }
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
  curve: number[]
  workout?: string
  ftpEstimate?: FtpEstimate
  segmentPrs: string[]
  group: boolean
}

export interface FtpEstimate {
  watts: number
  method: string
}

export interface WorkoutStep {
  kind: 'power' | 'free' | 'max'
  durationS: number
  from: number
  to: number
  cadence?: number
}

export interface Workout {
  id: string
  name: string
  description: string
  steps: WorkoutStep[]
  cues: { atS: number; text: string }[]
}

export interface WorkoutMeta {
  id: string
  name: string
  description: string
  durationS: number
  stress: number
  builtIn: boolean
  /** [seconds, fraction of FTP] corners. */
  shape: [number, number][]
}

export interface WorkoutPos {
  index: number
  kind: 'power' | 'free' | 'max'
  target: number
  stepLeftS: number
  cadence: number | null
}

export interface Segment {
  id: string
  name: string
  source: 'climb' | 'strava'
  start: number
  end: number
  avgGrade: number
  record?: string
}

export interface SegmentLive {
  id: string
  name: string
  source: 'climb' | 'strava'
  toGo: number
  length: number
  elapsed: number
  gap: number | null
  best: number | null
  record: string | null
}

export interface SegmentResult {
  id: string
  name: string
  source: 'climb' | 'strava'
  timeS: number
  previousBest: number | null
}

export interface GhostMeta {
  id: string
  name: string
  color: number
  distance: number
  timeS: number
}

export interface StravaActivity {
  id: number
  name: string
  start: string
  distance: number
  movingTime: number
}

export interface LoadDay {
  day: number
  tss: number
  fitness: number
  fatigue: number
  form: number
}

export interface FitnessOverview {
  durations: number[]
  curveAll: number[]
  curveRecent: number[]
  ftpEstimate: FtpEstimate | null
  load: LoadDay[]
  streaks: { weeks: number; bestWeeks: number; days: number }
  goal: { name: string; doneKm: number; targetKm: number; from: string; to: string; daysLeft: number; perWeekKm: number }
  weeks: { day: number; km: number; tss: number; hours: number }[]
}

export interface RideRequest {
  routeId: string
  workoutId?: string | null
  ghostIds?: string[]
  groupRoom?: string | null
}

export interface Profile {
  xp: number
  rides: number
  totalDistance: number
  totalAscent: number
  badges: string[]
  segmentPrs?: number
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
  kind: 'ghost' | 'bot' | 'remote'
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
  workout: Workout | null
  segments: Segment[]
  /** [lat, lon] of the route's middle. */
  centre: [number, number]
  /** Local hour when the ride started. */
  hour: number
  landmarks: Landmark[]
}

export interface Landmark {
  distance: number
  side: number
  kind: string
  name: string
  detail: string
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
  rivals: { id: string; name: string; kind: 'ghost' | 'bot' | 'remote'; color: number; distance: number; speed: number }[]
  demoTarget: number | null
  /** Seconds recorded so far; rides under 60 are not saved. */
  samples: number
  workout: WorkoutPos | null
  workoutTime: number
  workoutTarget: number | null
  intensity: number
  cue: string | null
  segment: SegmentLive | null
  segmentResult: SegmentResult | null
  groupStatus: string | null
}
