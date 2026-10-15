import * as THREE from 'three'
import type { TimeOfDay, Weather } from '../types'

/** Lighting and sky for one time of day. */
interface Light {
  zenith: number
  horizon: number
  hemiSky: number
  hemiGround: number
  hemi: number
  sun: number
  sunColor: number
  /** Direction towards the sun (or moon). */
  sunDir: [number, number, number]
  /** A lamp on the rider, for riding in the dark. */
  lamp: number
}

const TIMES: Record<Exclude<TimeOfDay, 'clock'>, Light> = {
  dawn: { zenith: 0x5a78b8, horizon: 0xf3b38a, hemiSky: 0xd8c8e8, hemiGround: 0x5a5a48, hemi: 1.3, sun: 1.8, sunColor: 0xffb27a, sunDir: [0.9, 0.18, 0.2], lamp: 0 },
  day: { zenith: 0x3f86d6, horizon: 0xbfd6e8, hemiSky: 0xe6f1ff, hemiGround: 0x6b7a4f, hemi: 2.0, sun: 2.4, sunColor: 0xfff1d6, sunDir: [0.5, 1, 0.35], lamp: 0 },
  evening: { zenith: 0x4c78b8, horizon: 0xf0cf9a, hemiSky: 0xf0e2c8, hemiGround: 0x6a6448, hemi: 1.6, sun: 2.2, sunColor: 0xffc88a, sunDir: [-0.8, 0.35, 0.3], lamp: 0 },
  dusk: { zenith: 0x23315e, horizon: 0xc8705a, hemiSky: 0x8a7fa8, hemiGround: 0x3a3630, hemi: 0.8, sun: 0.9, sunColor: 0xff8a5a, sunDir: [-0.9, 0.08, 0.25], lamp: 1.5 },
  night: { zenith: 0x060a18, horizon: 0x1a2236, hemiSky: 0x40507a, hemiGround: 0x101418, hemi: 0.45, sun: 0.35, sunColor: 0xa8c0ff, sunDir: [0.3, 0.8, -0.4], lamp: 3.0 }
}

/** Time of day from the clock. */
export function timeFromHour(hour: number): Exclude<TimeOfDay, 'clock'> {
  if (hour >= 5.5 && hour < 8) return 'dawn'
  if (hour >= 8 && hour < 17) return 'day'
  if (hour >= 17 && hour < 19) return 'evening'
  if (hour >= 19 && hour < 21) return 'dusk'
  return 'night'
}

/** WMO weather code (as Open-Meteo reports it) to the weather drawn. */
export function weatherFromCode(code: number): Exclude<Weather, 'live'> {
  if (code === 45 || code === 48) return 'fog'
  if ((code >= 71 && code <= 77) || code === 85 || code === 86) return 'snow'
  if ((code >= 51 && code <= 67) || (code >= 80 && code <= 82) || code >= 95) return 'rain'
  if (code >= 2) return 'cloudy'
  return 'clear'
}

export interface Look {
  zenith: THREE.Color
  horizon: THREE.Color
  fog: { color: THREE.Color; near: number; far: number }
  hemi: { sky: THREE.Color; ground: THREE.Color; intensity: number }
  sun: { color: THREE.Color; intensity: number; dir: THREE.Vector3 }
  lamp: number
  precipitation: 'none' | 'rain' | 'snow'
}

/** Combines a time of day with the weather into colours, light levels and haze. */
export function look(time: Exclude<TimeOfDay, 'clock'>, weather: Exclude<Weather, 'live'>): Look {
  const t = TIMES[time]
  const grey = new THREE.Color(time === 'night' ? 0x14181f : time === 'dusk' ? 0x4a4a52 : 0x9aa3ad)
  const overcast = weather === 'clear' ? 0 : weather === 'cloudy' ? 0.65 : weather === 'fog' ? 0.85 : 0.8
  const zenith = new THREE.Color(t.zenith).lerp(grey, overcast)
  const horizon = new THREE.Color(t.horizon).lerp(grey.clone().offsetHSL(0, 0, 0.06), overcast)
  const sunScale = weather === 'clear' ? 1 : weather === 'cloudy' ? 0.45 : 0.3
  const fog =
    weather === 'fog'
      ? { near: 10, far: 230 }
      : weather === 'rain' || weather === 'snow'
        ? { near: 60, far: 2200 }
        : weather === 'cloudy'
          ? { near: 200, far: 6000 }
          : { near: 300, far: 8500 }
  return {
    zenith,
    horizon,
    fog: { color: horizon.clone(), ...fog },
    hemi: { sky: new THREE.Color(t.hemiSky), ground: new THREE.Color(t.hemiGround), intensity: t.hemi * (weather === 'clear' ? 1 : 1.1) },
    sun: { color: new THREE.Color(t.sunColor), intensity: t.sun * sunScale, dir: new THREE.Vector3(...t.sunDir).normalize() },
    lamp: t.lamp + (weather === 'fog' || weather === 'rain' ? 0.6 : 0),
    precipitation: weather === 'rain' ? 'rain' : weather === 'snow' ? 'snow' : 'none'
  }
}

/** Falling rain or snow in a box that travels with the camera. */
export class Precipitation {
  readonly object: THREE.Object3D
  private positions: Float32Array
  private count: number
  private size = 70
  private height = 40

  constructor(private kind: 'rain' | 'snow') {
    this.count = kind === 'rain' ? 6000 : 4000
    const vertsPer = kind === 'rain' ? 2 : 1
    this.positions = new Float32Array(this.count * 3 * vertsPer)
    for (let i = 0; i < this.count; i++) this.seed(i, Math.random() * this.height)
    const geo = new THREE.BufferGeometry()
    geo.setAttribute('position', new THREE.BufferAttribute(this.positions, 3))
    if (kind === 'rain') {
      this.object = new THREE.LineSegments(geo, new THREE.LineBasicMaterial({ color: 0xaab8c8, transparent: true, opacity: 0.45, fog: false }))
    } else {
      this.object = new THREE.Points(geo, new THREE.PointsMaterial({ color: 0xffffff, size: 0.12, transparent: true, opacity: 0.9, fog: false }))
    }
    this.object.frustumCulled = false
  }

  private seed(i: number, y: number): void {
    const x = (Math.random() - 0.5) * this.size
    const z = (Math.random() - 0.5) * this.size
    if (this.kind === 'rain') {
      const k = i * 6
      this.positions.set([x, y, z, x + 0.02, y + 0.7, z], k)
    } else {
      this.positions.set([x, y, z], i * 3)
    }
  }

  /** Moves the flakes or drops on, centred on the camera. */
  update(dt: number, camera: THREE.Vector3, speed: number, heading: number): void {
    this.object.position.set(camera.x, camera.y - this.height / 2, camera.z)
    const fall = (this.kind === 'rain' ? 14 : 1.2) * dt
    // Riding into it: everything streams back past the rider.
    const back = speed * dt
    const bx = -Math.sin(heading) * back
    const bz = -Math.cos(heading) * back
    const half = this.size / 2
    const wrap = (v: number): number => (v < -half ? v + this.size : v > half ? v - this.size : v)
    const per = this.kind === 'rain' ? 2 : 1
    for (let i = 0; i < this.count; i++) {
      const k = i * 3 * per
      const y = this.positions[k + 1] - fall
      if (y < 0) {
        this.seed(i, this.height)
        continue
      }
      const sway = this.kind === 'snow' ? Math.sin(y * 0.7 + i) * 0.3 * dt : 0
      for (let v = 0; v < per; v++) {
        const o = k + v * 3
        this.positions[o] = wrap(this.positions[o] + bx + sway)
        this.positions[o + 1] -= fall
        this.positions[o + 2] = wrap(this.positions[o + 2] + bz)
      }
    }
    ;(this.object as THREE.Points).geometry.attributes.position.needsUpdate = true
  }

  dispose(): void {
    const o = this.object as THREE.Points
    o.geometry.dispose()
    ;(o.material as THREE.Material).dispose()
  }
}
