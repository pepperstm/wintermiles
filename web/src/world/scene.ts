import * as THREE from 'three'
import { api } from '../bridge'
import { sampleAt, type Route, type RoutePos } from '../core/gpx'
import type { Landmark, WorldLayout } from '../types'
import { RiderModel } from './rider'
import { Terrain } from './terrain'
import { packNative } from './native'
import { Precipitation, type Look } from './weather'

export interface WorldRival {
  id: string
  name: string
  color: number
  ghost: boolean
  distance: number
  speed: number
}

export interface WorldFrame {
  distance: number
  speed: number
  cadence: number
  rivals: WorldRival[]
}

const HORIZON = 0xbfd6e8
const ZENITH = 0x3f86d6

const CAMERAS = [
  { back: 7.5, up: 3.0, side: 0, lookAhead: 9, lookUp: 1.3 },
  { back: 24, up: 13, side: 0, lookAhead: 6, lookUp: 1 },
  { back: -1.5, up: 2.0, side: 9, lookAhead: 0, lookUp: 1.1 },
  // From high above, for seeing where you are in the map.
  { back: 38, up: 62, side: 0, lookAhead: 22, lookUp: 0 }
]

function textSprite(text: string, background: string, color = '#ffffff', border?: string): THREE.Sprite {
  const canvas = document.createElement('canvas')
  const ctx = canvas.getContext('2d')!
  const font = '600 44px system-ui, sans-serif'
  ctx.font = font
  const w = Math.ceil(ctx.measureText(text).width) + 48
  canvas.width = w
  canvas.height = 72
  ctx.font = font
  ctx.fillStyle = background
  ctx.beginPath()
  ctx.roundRect(0, 0, w, 72, 18)
  ctx.fill()
  if (border) {
    ctx.strokeStyle = border
    ctx.lineWidth = 8
    ctx.beginPath()
    ctx.roundRect(4, 4, w - 8, 64, 14)
    ctx.stroke()
  }
  ctx.fillStyle = color
  ctx.textBaseline = 'middle'
  ctx.fillText(text, 24, 38)
  const texture = new THREE.CanvasTexture(canvas)
  texture.colorSpace = THREE.SRGBColorSpace
  const sprite = new THREE.Sprite(new THREE.SpriteMaterial({ map: texture, transparent: true, depthWrite: false }))
  sprite.userData.aspect = w / 72
  return sprite
}

const POI_ICON: Record<string, string> = {
  cafe: '☕',
  restaurant: '🍴',
  pub: '🍺',
  water: '💧',
  bike_shop: '🔧',
  viewpoint: '👁',
  church: '⛪',
  castle: '🏰',
  monument: '🗿',
  museum: '🏛',
  hut: '🛖',
  attraction: '★'
}

export function roadTexture(): THREE.CanvasTexture {
  const canvas = document.createElement('canvas')
  canvas.width = 256
  canvas.height = 256
  const ctx = canvas.getContext('2d')!
  ctx.fillStyle = '#6d6552' // gravel shoulder, stretched down the embankment
  ctx.fillRect(0, 0, 256, 256)
  ctx.fillStyle = '#3d3f45'
  ctx.fillRect(20, 0, 216, 256)
  // Speckle so the surface visibly moves at speed.
  for (let i = 0; i < 2600; i++) {
    const shade = 50 + Math.random() * 40
    ctx.fillStyle = `rgba(${shade},${shade},${shade + 4},0.5)`
    ctx.fillRect(20 + Math.random() * 216, Math.random() * 256, 2, 2)
  }
  ctx.fillStyle = '#e8e8e2'
  ctx.fillRect(25, 0, 4, 256)
  ctx.fillRect(227, 0, 4, 256)
  ctx.fillRect(126, 0, 4, 128)
  const texture = new THREE.CanvasTexture(canvas)
  texture.colorSpace = THREE.SRGBColorSpace
  texture.wrapT = THREE.RepeatWrapping
  texture.wrapS = THREE.ClampToEdgeWrapping
  return texture
}

/** Wraps the road ribbon computed in Rust (`section` vertices across, per route sample) in a mesh. */
function buildRoad(buffer: ArrayBuffer, n: number, section: number, texture: THREE.Texture, dim: (sample: number) => number): THREE.Mesh {
  const verts = n * section
  const pos = new Float32Array(buffer, 0, verts * 3)
  const uv = new Float32Array(buffer, verts * 12, verts * 2)
  const nor = new Float32Array(verts * 3)
  for (let i = 1; i < nor.length; i += 3) nor[i] = 1
  // Daylight doesn't follow the road into a tunnel.
  const col = new Float32Array(verts * 3)
  for (let i = 0; i < n; i++) col.fill(dim(i), i * section * 3, (i + 1) * section * 3)
  const idx = new Uint32Array((n - 1) * (section - 1) * 6)
  let w = 0
  for (let i = 0; i < n - 1; i++) {
    for (let k = 0; k < section - 1; k++) {
      const a = i * section + k
      const b = a + section
      idx[w++] = a
      idx[w++] = b
      idx[w++] = a + 1
      idx[w++] = a + 1
      idx[w++] = b
      idx[w++] = b + 1
    }
  }
  const geo = new THREE.BufferGeometry()
  geo.setAttribute('position', new THREE.BufferAttribute(pos, 3))
  geo.setAttribute('normal', new THREE.BufferAttribute(nor, 3))
  geo.setAttribute('uv', new THREE.BufferAttribute(uv, 2))
  geo.setAttribute('color', new THREE.BufferAttribute(col, 3))
  geo.setIndex(new THREE.BufferAttribute(idx, 1))
  const mesh = new THREE.Mesh(geo, new THREE.MeshLambertMaterial({ map: texture, side: THREE.DoubleSide, vertexColors: true }))
  // One mesh spans the whole route, so its bounding sphere is useless for culling.
  mesh.frustumCulled = false
  return mesh
}

/**
 * Bridges, tunnels and whatever crosses overhead, computed in Rust: `[sunlit vertices,
 * unlit vertices]`, then sunlit positions, normals and colours; unlit positions and colours.
 */
function buildStructures(buffer: ArrayBuffer): THREE.Mesh[] {
  const [lit, unlit] = new Uint32Array(buffer, 0, 2)
  let offset = 8
  const take = (count: number): THREE.BufferAttribute => {
    const view = new Float32Array(buffer, offset, count * 3)
    offset += count * 12
    return new THREE.BufferAttribute(view, 3)
  }
  const meshes: THREE.Mesh[] = []
  if (lit) {
    const geo = new THREE.BufferGeometry()
    for (const name of ['position', 'normal', 'color']) geo.setAttribute(name, take(lit))
    meshes.push(new THREE.Mesh(geo, new THREE.MeshLambertMaterial({ vertexColors: true, side: THREE.DoubleSide })))
  }
  if (unlit) {
    const geo = new THREE.BufferGeometry()
    for (const name of ['position', 'color']) geo.setAttribute(name, take(unlit))
    meshes.push(new THREE.Mesh(geo, new THREE.MeshBasicMaterial({ vertexColors: true, side: THREE.DoubleSide })))
  }
  // Like the road, these span the whole route.
  for (const mesh of meshes) mesh.frustumCulled = false
  return meshes
}

function buildSky(): THREE.Mesh {
  const material = new THREE.ShaderMaterial({
    uniforms: { top: { value: new THREE.Color(ZENITH) }, bottom: { value: new THREE.Color(HORIZON) } },
    vertexShader: 'varying vec3 vDir; void main() { vDir = position; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }',
    fragmentShader: `uniform vec3 top; uniform vec3 bottom; varying vec3 vDir;
      void main() {
        float h = clamp(normalize(vDir).y, 0.0, 1.0);
        gl_FragColor = vec4(mix(bottom, top, pow(h, 0.55)), 1.0);
        #include <colorspace_fragment>
      }`,
    side: THREE.BackSide,
    depthWrite: false,
    depthTest: false,
    fog: false
  })
  const sky = new THREE.Mesh(new THREE.SphereGeometry(1000, 24, 12), material)
  sky.renderOrder = -1
  sky.frustumCulled = false
  return sky
}

interface Other {
  model: RiderModel
  label: THREE.Sprite
  lane: number
}

/** The 3D view of a ride: road, landscape, riders and a chase camera. */
export class World {
  private renderer: THREE.WebGLRenderer
  private scene = new THREE.Scene()
  private camera = new THREE.PerspectiveCamera(58, 1, 0.5, 20000)
  private sky = buildSky()
  private hemi = new THREE.HemisphereLight(0xe6f1ff, 0x6b7a4f, 2.0)
  private sun = new THREE.DirectionalLight(0xfff1d6, 2.4)
  private lamp = new THREE.PointLight(0xfff4e0, 0, 32, 1.6)
  private precipitation: Precipitation | null = null
  private terrain: Terrain
  private rider = new RiderModel(0xff5a36)
  private others = new Map<string, Other>()
  private pos = {} as RoutePos
  private otherPos = {} as RoutePos
  private camPos = {} as RoutePos
  private camHeading: number | null = null
  private camMode = 0
  private camInit = false
  private lookAt = new THREE.Vector3()

  constructor(
    canvas: HTMLCanvasElement,
    private route: Route,
    private layout: WorldLayout,
    /** Stretches of the route in a tunnel or under cover, as `[from, to]` in metres. */
    private covered: [number, number][],
    kmLabel: (km: number) => string,
    markerSpacing: number,
    private landmarks: Landmark[] = []
  ) {
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: 'high-performance' })
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2))
    // Long-range haze: nearby hills stay crisp, far ranges fade into the sky.
    this.scene.fog = new THREE.Fog(HORIZON, 300, 8500)
    this.scene.add(this.sky)
    this.scene.add(this.hemi)
    this.sun.position.set(0.5, 1, 0.35)
    this.scene.add(this.sun, this.lamp)

    this.terrain = new Terrain(layout)
    this.scene.add(this.terrain.group)
    this.scene.add(this.rider.group)
    this.addMarkers(kmLabel, markerSpacing)
  }

  /** Fetches the road and the landscape around the start; resolves when the first frame can be drawn. */
  async load(): Promise<void> {
    const start = sampleAt(this.route, 0)
    const [road, structures] = await Promise.all([api.ride.roadMesh(), api.ride.structures(), this.terrain.prime(start.x, start.z)])
    const texture = roadTexture()
    texture.anisotropy = this.renderer.capabilities.getMaxAnisotropy()
    const dim = (sample: number): number => 1 - 0.62 * this.cover(sample * this.route.step, 12)
    this.scene.add(buildRoad(road, this.route.n, this.layout.roadSection, texture, dim), ...buildStructures(structures))
  }

  /** Everything the native renderer needs from this scene (see `native.ts`). */
  nativeAssets(rivals: { id: string; name: string; kind: string }[], l: Look): Uint8Array {
    const tags = rivals.map((r) => {
      const sprite = textSprite(r.name, r.kind === 'ghost' ? 'rgba(255,255,255,0.25)' : 'rgba(20,24,32,0.7)')
      sprite.scale.set(sprite.userData.aspect, 1, 1)
      return { sprite, rider: r.id }
    })
    const skip = (o: THREE.Object3D): boolean => o === this.sky || o === this.terrain.group || o === this.rider.group || o === this.precipitation?.object
    return packNative(this.scene, skip, tags, l)
  }

  /** Sets the time of day and weather. */
  setLook(l: Look): void {
    const sky = this.sky.material as THREE.ShaderMaterial
    sky.uniforms.top.value.copy(l.zenith)
    sky.uniforms.bottom.value.copy(l.horizon)
    const fog = this.scene.fog as THREE.Fog
    fog.color.copy(l.fog.color)
    fog.near = l.fog.near
    fog.far = l.fog.far
    this.hemi.color.copy(l.hemi.sky)
    this.hemi.groundColor.copy(l.hemi.ground)
    this.hemi.intensity = l.hemi.intensity
    this.sun.color.copy(l.sun.color)
    this.sun.intensity = l.sun.intensity
    this.sun.position.copy(l.sun.dir)
    this.lamp.intensity = l.lamp * 14
    if (this.precipitation) {
      this.scene.remove(this.precipitation.object)
      this.precipitation.dispose()
      this.precipitation = null
    }
    if (l.precipitation !== 'none') {
      this.precipitation = new Precipitation(l.precipitation)
      this.scene.add(this.precipitation.object)
    }
  }

  /** Renders a frame and returns it as a PNG data URL (for the automated check). */
  capture(): string {
    this.renderer.render(this.scene, this.camera)
    return this.renderer.domElement.toDataURL('image/png')
  }

  /** How far under cover a point of the route is: 0 in the open, 1 once `fade` metres inside. */
  private cover(dist: number, fade: number): number {
    let deepest = 0
    for (const [from, to] of this.covered) deepest = Math.max(deepest, Math.min(dist - from, to - dist))
    return Math.max(0, Math.min(1, deepest / fade))
  }

  /** True if any of the route between `from` and `to` metres is under cover. */
  private underCover(from: number, to: number): boolean {
    return this.covered.some(([a, b]) => from < b && to > a)
  }

  private sideOfRoad(dist: number, side: number, height: number, out: THREE.Object3D): RoutePos {
    const p = sampleAt(this.route, dist)
    out.position.set(p.x + Math.cos(p.heading) * side, p.ele + height, p.z - Math.sin(p.heading) * side)
    return p
  }

  private addMarkers(kmLabel: (i: number) => string, spacing: number): void {
    const route = this.route
    const count = Math.min(400, Math.floor(route.total / spacing))
    if (count > 0) {
      const posts = new THREE.InstancedMesh(
        new THREE.CylinderGeometry(0.06, 0.06, 2.2, 6),
        new THREE.MeshLambertMaterial({ color: 0xe9e9e9 }),
        count
      )
      const holder = new THREE.Object3D()
      for (let i = 1; i <= count; i++) {
        this.sideOfRoad(i * spacing, this.layout.roadWidth / 2 + 1.6, 1.0, holder)
        holder.updateMatrix()
        posts.setMatrixAt(i - 1, holder.matrix)
        const sign = textSprite(kmLabel(i), 'rgba(20,24,32,0.85)')
        this.sideOfRoad(i * spacing, this.layout.roadWidth / 2 + 1.6, 2.6, sign)
        sign.scale.set(0.9 * sign.userData.aspect, 0.9, 1)
        this.scene.add(sign)
      }
      posts.instanceMatrix.needsUpdate = true
      posts.frustumCulled = false
      this.scene.add(posts)
    }

    route.climbs.forEach((c, i) => {
      const foot = textSprite(
        `▲ Climb ${i + 1} · ${(c.length / 1000).toFixed(1)} km · ${(c.avgGrade * 100).toFixed(1)}%`,
        'rgba(200,60,40,0.92)'
      )
      this.sideOfRoad(c.start, -this.layout.roadWidth / 2 - 2, 3.4, foot)
      foot.scale.set(1.3 * foot.userData.aspect, 1.3, 1)
      const named = this.landmarks.find((m) => m.kind === 'summit' && Math.abs(m.distance - c.end) < 1)
      const top = textSprite(`${named && !named.name.startsWith('Climb ') ? named.name : 'Summit'} · ${Math.round(route.ele[Math.round(c.end / route.step)])} m`, 'rgba(40,150,90,0.92)')
      this.sideOfRoad(c.end, -this.layout.roadWidth / 2 - 2, 3.4, top)
      top.scale.set(1.3 * top.userData.aspect, 1.3, 1)
      this.scene.add(foot, top)
    })

    this.addLandmarks()
    this.addArch(Math.min(12, route.total), 0x2f7fd8, 'START')
    this.addArch(route.total - 4, 0xd8382f, 'FINISH')
  }

  /** Place-name signs on posts where the road enters a village or town, and roadside points of interest. */
  private addLandmarks(): void {
    const posts: THREE.Matrix4[] = []
    const holder = new THREE.Object3D()
    for (const m of this.landmarks) {
      if (m.kind === 'summit') continue
      const place = ['city', 'town', 'village', 'hamlet', 'suburb', 'locality'].includes(m.kind)
      const off = (this.layout.roadWidth / 2 + (place ? 2.2 : 3.2)) * m.side
      const sign = place ? textSprite(m.name, '#ffffff', '#1a1a1a', '#d42020') : textSprite(`${POI_ICON[m.kind] ?? '•'} ${m.name}`, 'rgba(30,70,140,0.9)')
      const height = place ? 2.3 : 2.0
      this.sideOfRoad(m.distance, off, height, sign)
      const size = place ? (m.kind === 'city' || m.kind === 'town' ? 1.5 : 1.2) : 1.0
      sign.scale.set(size * sign.userData.aspect, size, 1)
      this.scene.add(sign)
      this.sideOfRoad(m.distance, off, (height - size / 2) / 2, holder)
      holder.scale.set(1, height - size / 2, 1)
      holder.updateMatrix()
      posts.push(holder.matrix.clone())
    }
    if (!posts.length) return
    const mesh = new THREE.InstancedMesh(new THREE.CylinderGeometry(0.05, 0.05, 1, 6), new THREE.MeshLambertMaterial({ color: 0x9aa0a6 }), posts.length)
    posts.forEach((m, i) => mesh.setMatrixAt(i, m))
    mesh.instanceMatrix.needsUpdate = true
    mesh.frustumCulled = false
    this.scene.add(mesh)
  }

  private addArch(dist: number, color: number, text: string): void {
    const p = sampleAt(this.route, dist)
    const arch = new THREE.Group()
    const mat = new THREE.MeshLambertMaterial({ color })
    const half = this.layout.roadWidth / 2 + 0.6
    for (const side of [-1, 1]) {
      const post = new THREE.Mesh(new THREE.CylinderGeometry(0.22, 0.22, 5.2, 10), mat)
      post.position.set(side * half, 2.6, 0)
      arch.add(post)
    }
    const beam = new THREE.Mesh(new THREE.BoxGeometry(half * 2 + 0.44, 1.1, 0.5), mat)
    beam.position.y = 5.2
    arch.add(beam)
    arch.position.set(p.x, p.ele, p.z)
    arch.rotation.y = p.heading
    const label = textSprite(text, 'rgba(0,0,0,0)')
    label.position.set(p.x, p.ele + 5.2, p.z)
    label.scale.set(0.95 * label.userData.aspect, 0.95, 1)
    label.renderOrder = 2
    ;(label.material as THREE.SpriteMaterial).depthTest = false
    this.scene.add(arch, label)
  }

  cycleCamera(): void {
    this.camMode = (this.camMode + 1) % CAMERAS.length
  }

  resize(width: number, height: number): void {
    if (width < 2 || height < 2) return
    this.renderer.setSize(width, height, false)
    this.camera.aspect = width / height
    this.camera.updateProjectionMatrix()
  }

  private place(group: THREE.Object3D, p: RoutePos, lane: number): void {
    group.position.set(p.x + Math.cos(p.heading) * lane, p.ele + this.layout.roadLift, p.z - Math.sin(p.heading) * lane)
    group.rotation.set(-Math.atan(p.grade), p.heading, 0, 'YXZ')
  }

  frame(dt: number, f: WorldFrame): void {
    const p = sampleAt(this.route, f.distance, this.pos)
    this.place(this.rider.group, p, 0)
    this.rider.update(dt, f.speed, f.cadence)

    const seen = new Set<string>()
    f.rivals.forEach((r, i) => {
      seen.add(r.id)
      let other = this.others.get(r.id)
      if (!other) {
        const label = textSprite(r.name, r.ghost ? 'rgba(255,255,255,0.25)' : 'rgba(20,24,32,0.7)')
        other = { model: new RiderModel(r.color, r.ghost), label, lane: (i % 2 ? 1 : -1) * (1.3 + 0.5 * Math.floor(i / 2)) }
        this.others.set(r.id, other)
        this.scene.add(other.model.group, label)
      }
      const gap = Math.abs(r.distance - f.distance)
      const visible = gap < 700 && r.distance > 0 && r.distance < this.route.total
      other.model.group.visible = visible
      other.label.visible = visible && gap < 250 && gap > 12
      if (!visible) return
      const op = sampleAt(this.route, r.distance, this.otherPos)
      this.place(other.model.group, op, other.lane)
      other.model.update(dt, r.speed, r.speed > 0.5 ? 85 : 0)
      other.label.position.copy(other.model.group.position)
      other.label.position.y += 2.3
      // Scale with distance from the camera so labels stay a steady, modest size on screen.
      const size = Math.max(0.3, Math.min(3, other.label.position.distanceTo(this.camera.position) / 55))
      other.label.scale.set(size * other.label.userData.aspect, size, 1)
    })
    for (const [id, other] of this.others) {
      if (seen.has(id)) continue
      this.scene.remove(other.model.group, other.label)
      other.model.dispose()
      this.others.delete(id)
    }

    this.terrain.update(p.x, p.z)
    this.updateCamera(dt, p, f.distance)
    this.sky.position.copy(this.camera.position)
    // The rider's lamp, just ahead and above, lights the road at night.
    this.lamp.position.set(p.x + Math.sin(p.heading) * 6, p.ele + 3, p.z + Math.cos(p.heading) * 6)
    this.precipitation?.update(dt, this.camera.position, f.speed, p.heading)
    this.renderer.render(this.scene, this.camera)
  }

  private updateCamera(dt: number, p: RoutePos, dist: number): void {
    // Ease the camera's heading so hairpins swing round rather than snap.
    if (this.camHeading === null) this.camHeading = p.heading
    let dh = p.heading - this.camHeading
    dh = Math.atan2(Math.sin(dh), Math.cos(dh))
    this.camHeading += dh * (1 - Math.exp(-dt * 2.2))
    const fx = Math.sin(this.camHeading)
    const fz = Math.cos(this.camHeading)
    let c = CAMERAS[this.camMode]
    let tx = p.x - fx * c.back + fz * c.side
    let tz = p.z - fz * c.back - fx * c.side
    let ty = p.ele + c.up
    // In a tunnel there is only one place for the camera: on the road, close behind
    // the rider. It drops in a little before the mouth and stays until it is out again.
    const back = Math.max(c.back, CAMERAS[0].back)
    if (this.underCover(dist - back - 6, dist + 30)) {
      c = CAMERAS[0]
      const behind = sampleAt(this.route, Math.max(0, dist - c.back), this.camPos)
      tx = behind.x
      tz = behind.z
      ty = behind.ele + c.up
    }
    // Never let a hillside swallow the camera.
    ty = Math.max(ty, (this.terrain.heightAt(tx, tz) ?? -Infinity) + 1.4)
    const k = this.camInit ? 1 - Math.exp(-dt * 4) : 1
    this.camInit = true
    const cam = this.camera.position
    cam.set(cam.x + (tx - cam.x) * k, cam.y + (ty - cam.y) * k, cam.z + (tz - cam.z) * k)
    const lx = p.x + fx * c.lookAhead
    const lz = p.z + fz * c.lookAhead
    const ly = p.ele + c.lookUp + p.grade * c.lookAhead
    this.lookAt.set(this.lookAt.x + (lx - this.lookAt.x) * k, this.lookAt.y + (ly - this.lookAt.y) * k, this.lookAt.z + (lz - this.lookAt.z) * k)
    this.camera.lookAt(this.lookAt)
  }

  dispose(): void {
    this.terrain.dispose()
    this.rider.dispose()
    this.precipitation?.dispose()
    for (const other of this.others.values()) other.model.dispose()
    this.scene.traverse((obj) => {
      const mesh = obj as THREE.Mesh
      mesh.geometry?.dispose()
      const materials = Array.isArray(mesh.material) ? mesh.material : mesh.material ? [mesh.material] : []
      for (const m of materials) {
        ;(m as THREE.MeshLambertMaterial).map?.dispose()
        m.dispose()
      }
    })
    this.renderer.dispose()
  }
}
