import * as THREE from 'three'
import { mergeGeometries } from 'three/examples/jsm/utils/BufferGeometryUtils.js'
import { api } from '../bridge'
import type { WorldLayout } from '../types'

// The landscape is computed in Rust, off the render thread, and arrives as
// ready-made vertex buffers: square chunks around the rider plus one coarse
// sheet for the distant mountains. This class only decides what to ask for and
// uploads the answers to the GPU.

const MAX_IN_FLIGHT = 6

function buildTreeGeometry(): THREE.BufferGeometry {
  const paint = (g: THREE.BufferGeometry, r: number, gr: number, b: number): THREE.BufferGeometry => {
    const n = g.getAttribute('position').count
    const colors = new Float32Array(n * 3)
    for (let i = 0; i < n; i++) colors.set([r, gr, b], i * 3)
    g.setAttribute('color', new THREE.BufferAttribute(colors, 3))
    return g.toNonIndexed()
  }
  const trunk = paint(new THREE.CylinderGeometry(0.22, 0.3, 1.8, 5).translate(0, 0.9, 0), 0.32, 0.22, 0.14)
  const lower = paint(new THREE.ConeGeometry(2.1, 4.2, 7).translate(0, 3.6, 0), 0.13, 0.36, 0.17)
  const upper = paint(new THREE.ConeGeometry(1.5, 3.6, 7).translate(0, 6.0, 0), 0.16, 0.42, 0.2)
  return mergeGeometries([trunk, lower, upper])!
}

/**
 * One window bay of one storey. It multiplies the wall colour, so it is white
 * except for the window itself; the bottom-left corner is plain wall, which
 * roofs and gables point at.
 */
function windowTexture(): THREE.CanvasTexture {
  const canvas = document.createElement('canvas')
  canvas.width = 64
  canvas.height = 64
  const ctx = canvas.getContext('2d')!
  ctx.fillStyle = '#ffffff'
  ctx.fillRect(0, 0, 64, 64)
  ctx.fillStyle = '#e2ddd4' // frame and sill
  ctx.fillRect(19, 13, 26, 36)
  ctx.fillStyle = '#3b4656' // glass
  ctx.fillRect(22, 16, 20, 29)
  ctx.fillStyle = '#566477' // a hint of sky reflected in the upper pane
  ctx.fillRect(22, 16, 20, 9)
  ctx.fillStyle = '#e2ddd4'
  ctx.fillRect(31, 16, 2, 29)
  const texture = new THREE.CanvasTexture(canvas)
  texture.colorSpace = THREE.SRGBColorSpace
  texture.wrapS = texture.wrapT = THREE.RepeatWrapping
  texture.anisotropy = 4
  return texture
}

/**
 * Surface of the roads around the route. The left three-eighths is tarmac with
 * a broken centre line, for main roads (their edge lines are geometry); the rest
 * is blank, for everything whose colour comes from its vertices alone.
 */
function waysTexture(): THREE.CanvasTexture {
  const canvas = document.createElement('canvas')
  canvas.width = 128
  canvas.height = 64
  const ctx = canvas.getContext('2d')!
  ctx.fillStyle = '#ffffff'
  ctx.fillRect(0, 0, 128, 64)
  // The tarmac runs on past the road's edge, so that distant (blurred) roads don't pick up the blank part.
  ctx.fillStyle = '#3d3f45'
  ctx.fillRect(0, 0, 80, 64)
  ctx.fillStyle = '#e8e8e2'
  ctx.fillRect(23, 0, 2, 22)
  const texture = new THREE.CanvasTexture(canvas)
  texture.colorSpace = THREE.SRGBColorSpace
  texture.wrapT = THREE.RepeatWrapping
  texture.wrapS = THREE.ClampToEdgeWrapping
  texture.anisotropy = 8
  return texture
}

function gridIndices(cells: number): Uint16Array {
  const row = cells + 1
  const idx: number[] = []
  for (let j = 0; j < cells; j++) {
    for (let i = 0; i < cells; i++) {
      const a = j * row + i
      idx.push(a, a + row, a + 1, a + 1, a + row, a + row + 1)
    }
  }
  return new Uint16Array(idx)
}

interface GridReply {
  positions: Float32Array
  normals: Float32Array
  colors: Float32Array
  trees: Float32Array
  /** Building triangles: positions, normals and colours (three floats per vertex), then UVs (two). */
  buildings: [Float32Array, Float32Array, Float32Array, Float32Array] | null
  /** Water triangles: positions and colours. */
  water: [Float32Array, Float32Array] | null
  /** Triangles of the roads around the route: positions, normals, colours and UVs. */
  roads: [Float32Array, Float32Array, Float32Array, Float32Array] | null
}

/**
 * Splits a mesh reply: `[ground vertices, trees, building vertices, water vertices,
 * road vertices]`, then ground positions, normals and colours; trees; building
 * positions, normals, colours and UVs; water positions and colours; road positions,
 * normals, colours and UVs.
 */
function decodeGrid(buffer: ArrayBuffer): GridReply {
  const [verts, trees, walls, water, roads] = new Uint32Array(buffer, 0, 5)
  let offset = 20
  const take = (length: number): Float32Array => {
    const view = new Float32Array(buffer, offset, length)
    offset += length * 4
    return view
  }
  return {
    positions: take(verts * 3),
    normals: take(verts * 3),
    colors: take(verts * 3),
    trees: take(trees * 6),
    buildings: walls ? [take(walls * 3), take(walls * 3), take(walls * 3), take(walls * 2)] : null,
    water: water ? [take(water * 3), take(water * 3)] : null,
    roads: roads ? [take(roads * 3), take(roads * 3), take(roads * 3), take(roads * 2)] : null
  }
}

interface Chunk {
  mesh: THREE.Mesh
  trees: THREE.InstancedMesh | null
  buildings: THREE.Mesh | null
  water: THREE.Mesh | null
  roads: THREE.Mesh | null
  /** Vertex positions, kept for ground-height lookups. */
  positions: Float32Array
}

const key = (cx: number, cz: number): string => `${cx},${cz}`

export class Terrain {
  readonly group = new THREE.Group()
  private chunks = new Map<string, Chunk>()
  private pending = new Set<string>()
  private indices: Uint16Array
  private ground = new THREE.MeshLambertMaterial({ vertexColors: true })
  private treeGeometry = buildTreeGeometry()
  private treeMaterial = new THREE.MeshLambertMaterial({ vertexColors: true, flatShading: true })
  private buildingMaterial = new THREE.MeshLambertMaterial({ vertexColors: true, map: windowTexture() })
  // Slightly see-through, with a sheen where the sun catches it.
  private waterMaterial = new THREE.MeshPhongMaterial({ vertexColors: true, transparent: true, opacity: 0.86, shininess: 90, specular: 0x8899aa })
  // Roads lie on the ground and are painted over it, lesser ways first: they are drawn after
  // everything solid and leave the depth buffer alone, so they can't flicker against the
  // ground or against each other where they meet.
  private roadMaterial = new THREE.MeshLambertMaterial({ vertexColors: true, map: waysTexture(), depthWrite: false, polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -4 })
  private far: THREE.Mesh
  private farAt = ''
  private farBusy = false
  private centre = { cx: 0, cz: 0 }
  private disposed = false

  constructor(private layout: WorldLayout) {
    this.indices = gridIndices(layout.cells)
    const farGeo = new THREE.BufferGeometry()
    const farVerts = (layout.farCells + 1) ** 2
    for (const name of ['position', 'normal', 'color']) farGeo.setAttribute(name, new THREE.BufferAttribute(new Float32Array(farVerts * 3), 3))
    farGeo.setIndex(Array.from(gridIndices(layout.farCells)))
    this.far = new THREE.Mesh(farGeo, this.ground)
    this.far.frustumCulled = false
    this.far.visible = false
    this.group.add(this.far)
  }

  /** Loads the landscape immediately around a point and resolves once it is all in place. */
  async prime(x: number, z: number): Promise<void> {
    const { chunk } = this.layout
    const cx = Math.floor(x / chunk)
    const cz = Math.floor(z / chunk)
    this.centre = { cx, cz }
    const jobs: Promise<void>[] = [this.loadFar(x, z)]
    for (let dx = -2; dx <= 2; dx++) for (let dz = -2; dz <= 2; dz++) jobs.push(this.load(cx + dx, cz + dz))
    await Promise.all(jobs)
  }

  /** Requests whatever is missing around (x, z), nearest first, and drops what is far behind. */
  update(x: number, z: number): void {
    const { chunk, viewChunks: view } = this.layout
    const cx = Math.floor(x / chunk)
    const cz = Math.floor(z / chunk)
    this.centre = { cx, cz }
    void this.loadFar(x, z)

    outer: for (let ring = 0; ring <= view; ring++) {
      for (let dx = -ring; dx <= ring; dx++) {
        for (let dz = -ring; dz <= ring; dz++) {
          if (Math.max(Math.abs(dx), Math.abs(dz)) !== ring || dx * dx + dz * dz > (view + 0.5) ** 2) continue
          if (this.pending.size >= MAX_IN_FLIGHT) break outer
          const k = key(cx + dx, cz + dz)
          if (!this.chunks.has(k) && !this.pending.has(k)) void this.load(cx + dx, cz + dz)
        }
      }
    }
    for (const [k, c] of this.chunks) {
      const [kx, kz] = k.split(',').map(Number)
      if (Math.abs(kx - cx) > view + 2 || Math.abs(kz - cz) > view + 2) {
        this.release(c)
        this.chunks.delete(k)
      }
    }
  }

  private async load(cx: number, cz: number): Promise<void> {
    const k = key(cx, cz)
    if (this.chunks.has(k) || this.pending.has(k)) return
    this.pending.add(k)
    try {
      const data = decodeGrid(await api.ride.chunk(cx, cz))
      // By the time it arrives the rider may have moved on, or the ride may be over.
      const view = this.layout.viewChunks + 2
      if (this.disposed || Math.abs(cx - this.centre.cx) > view || Math.abs(cz - this.centre.cz) > view) return
      const geo = new THREE.BufferGeometry()
      geo.setAttribute('position', new THREE.BufferAttribute(data.positions, 3))
      geo.setAttribute('normal', new THREE.BufferAttribute(data.normals, 3))
      geo.setAttribute('color', new THREE.BufferAttribute(data.colors, 3))
      // Each chunk gets its own index attribute: sharing one would free its GPU buffer
      // for every chunk the first time any chunk is disposed.
      geo.setIndex(new THREE.BufferAttribute(this.indices, 1))
      geo.computeBoundingSphere()
      const mesh = new THREE.Mesh(geo, this.ground)
      this.group.add(mesh)

      let trees: THREE.InstancedMesh | null = null
      const count = data.trees.length / 6
      if (count) {
        trees = new THREE.InstancedMesh(this.treeGeometry, this.treeMaterial, count)
        const holder = new THREE.Object3D()
        for (let i = 0; i < count; i++) {
          const [x, y, z, yaw, scale, tall] = data.trees.subarray(i * 6, i * 6 + 6)
          holder.position.set(x, y, z)
          holder.rotation.set(0, yaw, 0)
          holder.scale.set(scale, tall, scale)
          holder.updateMatrix()
          trees.setMatrixAt(i, holder.matrix)
        }
        trees.instanceMatrix.needsUpdate = true
        trees.computeBoundingSphere()
        this.group.add(trees)
      }
      let buildings: THREE.Mesh | null = null
      if (data.buildings) {
        const walls = new THREE.BufferGeometry()
        walls.setAttribute('position', new THREE.BufferAttribute(data.buildings[0], 3))
        walls.setAttribute('normal', new THREE.BufferAttribute(data.buildings[1], 3))
        walls.setAttribute('color', new THREE.BufferAttribute(data.buildings[2], 3))
        walls.setAttribute('uv', new THREE.BufferAttribute(data.buildings[3], 2))
        walls.computeBoundingSphere()
        buildings = new THREE.Mesh(walls, this.buildingMaterial)
        this.group.add(buildings)
      }
      let water: THREE.Mesh | null = null
      if (data.water) {
        const surface = new THREE.BufferGeometry()
        surface.setAttribute('position', new THREE.BufferAttribute(data.water[0], 3))
        surface.setAttribute('color', new THREE.BufferAttribute(data.water[1], 3))
        // Still water is as good as level; one upward normal serves every vertex.
        const up = new Float32Array(data.water[0].length)
        for (let i = 1; i < up.length; i += 3) up[i] = 1
        surface.setAttribute('normal', new THREE.BufferAttribute(up, 3))
        surface.computeBoundingSphere()
        water = new THREE.Mesh(surface, this.waterMaterial)
        this.group.add(water)
      }
      let roads: THREE.Mesh | null = null
      if (data.roads) {
        const ways = new THREE.BufferGeometry()
        ways.setAttribute('position', new THREE.BufferAttribute(data.roads[0], 3))
        ways.setAttribute('normal', new THREE.BufferAttribute(data.roads[1], 3))
        ways.setAttribute('color', new THREE.BufferAttribute(data.roads[2], 3))
        ways.setAttribute('uv', new THREE.BufferAttribute(data.roads[3], 2))
        ways.computeBoundingSphere()
        roads = new THREE.Mesh(ways, this.roadMaterial)
        roads.renderOrder = 1
        this.group.add(roads)
      }
      this.chunks.set(k, { mesh, trees, buildings, water, roads, positions: data.positions })
    } catch (err) {
      if (!this.disposed) void api.log(`terrain chunk ${k} failed: ${err}`)
    } finally {
      this.pending.delete(k)
    }
  }

  private async loadFar(x: number, z: number): Promise<void> {
    const cell = this.layout.farCell
    // Snap the sheet to its own grid so its vertices don't swim as the rider moves.
    const cx = Math.round(x / cell) * cell
    const cz = Math.round(z / cell) * cell
    if (this.farBusy || this.farAt === key(cx, cz)) return
    this.farBusy = true
    try {
      const data = decodeGrid(await api.ride.far(cx, cz))
      if (this.disposed) return
      const geo = this.far.geometry
      for (const [name, values] of [['position', data.positions], ['normal', data.normals], ['color', data.colors]] as const) {
        const attr = geo.getAttribute(name) as THREE.BufferAttribute
        ;(attr.array as Float32Array).set(values)
        attr.needsUpdate = true
      }
      this.far.visible = true
      this.farAt = key(cx, cz)
    } catch (err) {
      if (!this.disposed) void api.log(`far terrain failed: ${err}`)
    } finally {
      this.farBusy = false
    }
  }

  /** Ground height under a point, from the loaded chunk there; `null` if that chunk isn't in yet. */
  heightAt(x: number, z: number): number | null {
    const { chunk, cells } = this.layout
    const cx = Math.floor(x / chunk)
    const cz = Math.floor(z / chunk)
    const c = this.chunks.get(key(cx, cz))
    if (!c) return null
    const fx = (x / chunk - cx) * cells
    const fz = (z / chunk - cz) * cells
    const i = Math.min(cells - 1, Math.floor(fx))
    const j = Math.min(cells - 1, Math.floor(fz))
    const u = fx - i
    const v = fz - j
    const h = (ii: number, jj: number): number => c.positions[(jj * (cells + 1) + ii) * 3 + 1]
    return (h(i, j) * (1 - u) + h(i + 1, j) * u) * (1 - v) + (h(i, j + 1) * (1 - u) + h(i + 1, j + 1) * u) * v
  }

  private release(c: Chunk): void {
    this.group.remove(c.mesh)
    c.mesh.geometry.dispose()
    if (c.trees) {
      this.group.remove(c.trees)
      c.trees.dispose()
    }
    if (c.buildings) {
      this.group.remove(c.buildings)
      c.buildings.geometry.dispose()
    }
    if (c.water) {
      this.group.remove(c.water)
      c.water.geometry.dispose()
    }
    if (c.roads) {
      this.group.remove(c.roads)
      c.roads.geometry.dispose()
    }
  }

  dispose(): void {
    this.disposed = true
    for (const c of this.chunks.values()) this.release(c)
    this.chunks.clear()
    this.far.geometry.dispose()
    this.ground.dispose()
    this.treeGeometry.dispose()
    this.treeMaterial.dispose()
    this.buildingMaterial.map?.dispose()
    this.buildingMaterial.dispose()
    this.waterMaterial.dispose()
    this.roadMaterial.map?.dispose()
    this.roadMaterial.dispose()
  }
}
