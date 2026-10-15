import * as THREE from 'three'
import { RiderModel } from './rider'
import { roadTexture } from './scene'
import { waysTexture, windowTexture } from './terrain'
import type { Look } from './weather'

/**
 * Packs what the native renderer can't build itself — textures, text signs, props along
 * the route and the rider in a few pedal positions — into one buffer:
 * `[u32 header length][JSON header][data…]`, each data block starting on a 4-byte boundary.
 */

const POSES = 6
const FLOATS_PER_VERTEX = 11

interface LabelDef {
  at: [number, number, number]
  rider: string | null
  size: [number, number]
  uv: [number, number, number, number]
}

/** RGBA rows of a canvas, bottom row first (as three.js uploads them, with flipY). */
function pixels(canvas: HTMLCanvasElement, flip: boolean): Uint8Array {
  const data = canvas.getContext('2d')!.getImageData(0, 0, canvas.width, canvas.height).data
  if (!flip) return new Uint8Array(data.buffer.slice(0))
  const out = new Uint8Array(data.length)
  const row = canvas.width * 4
  for (let y = 0; y < canvas.height; y++) out.set(data.subarray(y * row, (y + 1) * row), (canvas.height - 1 - y) * row)
  return out
}

/** Appends a mesh's triangles, in the space `matrix` takes them to, with colours. */
function bake(out: number[], geometry: THREE.BufferGeometry, matrix: THREE.Matrix4, tint: THREE.Color): void {
  const g = geometry.index ? geometry.toNonIndexed() : geometry
  const pos = g.getAttribute('position')
  let nor = g.getAttribute('normal')
  if (!nor) {
    g.computeVertexNormals()
    nor = g.getAttribute('normal')
  }
  const col = g.getAttribute('color')
  const normalMatrix = new THREE.Matrix3().getNormalMatrix(matrix)
  const p = new THREE.Vector3()
  const n = new THREE.Vector3()
  for (let i = 0; i < pos.count; i++) {
    p.fromBufferAttribute(pos, i).applyMatrix4(matrix)
    n.fromBufferAttribute(nor, i).applyMatrix3(normalMatrix).normalize()
    const r = (col ? col.getX(i) : 1) * tint.r
    const gr = (col ? col.getY(i) : 1) * tint.g
    const b = (col ? col.getZ(i) : 1) * tint.b
    out.push(p.x, p.y, p.z, n.x, n.y, n.z, r, gr, b, 0.5, 0.5)
  }
  if (g !== geometry) g.dispose()
}

/** Every vertex-coloured mesh under `root`, baked into `root`'s space. */
function bakeTree(root: THREE.Object3D, skip: (o: THREE.Object3D) => boolean): number[] {
  const out: number[] = []
  root.updateMatrixWorld(true)
  const inverse = root.matrixWorld.clone().invert()
  const visit = (o: THREE.Object3D): void => {
    if (skip(o)) return
    const mesh = o as THREE.Mesh
    if (mesh.isMesh && !(mesh.material instanceof THREE.MeshBasicMaterial)) {
      const mat = mesh.material as THREE.MeshLambertMaterial
      const tint = mat.vertexColors ? new THREE.Color(1, 1, 1) : mat.color.clone()
      const inst = o as THREE.InstancedMesh
      if (inst.isInstancedMesh) {
        const m = new THREE.Matrix4()
        for (let i = 0; i < inst.count; i++) {
          inst.getMatrixAt(i, m)
          bake(out, mesh.geometry, new THREE.Matrix4().multiplyMatrices(inverse, o.matrixWorld).multiply(m), tint)
        }
      } else {
        bake(out, mesh.geometry, new THREE.Matrix4().multiplyMatrices(inverse, o.matrixWorld), tint)
      }
    }
    o.children.forEach(visit)
  }
  root.children.forEach(visit)
  return out
}

/** The rider in `POSES` pedal positions. The jersey's share of each vertex colour goes in u. */
function riderPoses(): number[][] {
  const white = new RiderModel(0xffffff)
  const black = new RiderModel(0x000000)
  const poses: number[][] = []
  for (let k = 0; k < POSES; k++) {
    const a = bakeTree(white.group, () => false)
    const b = bakeTree(black.group, () => false)
    for (let i = 0; i < a.length; i += FLOATS_PER_VERTEX) {
      // Colour = black's colour + weight × jersey colour; the weight is how much white lifted it.
      const w = (a[i + 6] - b[i + 6] + a[i + 7] - b[i + 7] + a[i + 8] - b[i + 8]) / 3
      b[i + 9] = Math.max(0, w)
      b[i + 10] = 0
    }
    poses.push(b)
    // One sixth of a turn: at 60 rpm, a sixth of a second.
    white.update(1 / POSES, 0, 60)
    black.update(1 / POSES, 0, 60)
  }
  white.dispose()
  black.dispose()
  return poses
}

/** Packs every sprite's canvas into one atlas, at half resolution. */
function labelAtlas(sprites: { sprite: THREE.Sprite; rider: string | null }[]): { canvas: HTMLCanvasElement; labels: LabelDef[] } {
  const W = 2048
  const scale = 0.5
  let x = 0
  let y = 0
  let rowH = 0
  const places: { sx: number; sy: number; w: number; h: number }[] = []
  for (const { sprite } of sprites) {
    const img = (sprite.material as THREE.SpriteMaterial).map!.image as HTMLCanvasElement
    const w = Math.ceil(img.width * scale)
    const h = Math.ceil(img.height * scale)
    if (x + w > W) {
      x = 0
      y += rowH + 2
      rowH = 0
    }
    places.push({ sx: x, sy: y, w, h })
    x += w + 2
    rowH = Math.max(rowH, h)
  }
  const H = Math.min(8192, Math.max(4, 2 ** Math.ceil(Math.log2(y + rowH + 2))))
  const canvas = document.createElement('canvas')
  canvas.width = W
  canvas.height = H
  const ctx = canvas.getContext('2d')!
  const labels: LabelDef[] = []
  sprites.forEach(({ sprite, rider }, i) => {
    const pl = places[i]
    if (pl.sy + pl.h > H) return
    const img = (sprite.material as THREE.SpriteMaterial).map!.image as HTMLCanvasElement
    ctx.drawImage(img, pl.sx, pl.sy, pl.w, pl.h)
    labels.push({
      at: [sprite.position.x, sprite.position.y, sprite.position.z],
      rider,
      size: [sprite.scale.x, sprite.scale.y],
      uv: [(pl.sx + 0.5) / W, (pl.sy + 0.5) / H, (pl.sx + pl.w - 0.5) / W, (pl.sy + pl.h - 0.5) / H]
    })
  })
  return { canvas, labels }
}

/** Look → the renderer's linear colours with intensities applied. */
export function nativeLook(l: Look, lampScale = 14): Record<string, unknown> {
  const rgb = (c: THREE.Color, k = 1): number[] => [c.r * k, c.g * k, c.b * k]
  return {
    zenith: rgb(l.zenith),
    horizon: rgb(l.horizon),
    fogColor: rgb(l.fog.color),
    fogNear: l.fog.near,
    fogFar: l.fog.far,
    hemiSky: rgb(l.hemi.sky, l.hemi.intensity),
    hemiGround: rgb(l.hemi.ground, l.hemi.intensity),
    sunColor: rgb(l.sun.color, l.sun.intensity),
    sunDir: [l.sun.dir.x, l.sun.dir.y, l.sun.dir.z],
    lamp: l.lamp * lampScale
  }
}

export function packNative(scene: THREE.Scene, skip: (o: THREE.Object3D) => boolean, riderSprites: { sprite: THREE.Sprite; rider: string }[], look: Look): Uint8Array {
  const sprites: { sprite: THREE.Sprite; rider: string | null }[] = []
  scene.traverse((o) => {
    if ((o as THREE.Sprite).isSprite && !skip(o)) sprites.push({ sprite: o as THREE.Sprite, rider: null })
  })
  sprites.push(...riderSprites)
  const atlas = labelAtlas(sprites)
  const props = bakeTree(scene, (o) => skip(o) || (o as THREE.Sprite).isSprite === true)
  const poses = riderPoses()

  const textures: { name: string; canvas: HTMLCanvasElement; wrap: string; flip: boolean }[] = [
    { name: 'road', canvas: roadTexture().image as HTMLCanvasElement, wrap: 'clampS', flip: true },
    { name: 'windows', canvas: windowTexture().image as HTMLCanvasElement, wrap: 'repeat', flip: true },
    { name: 'ways', canvas: waysTexture().image as HTMLCanvasElement, wrap: 'clampS', flip: true },
    { name: 'labels', canvas: atlas.canvas, wrap: 'clamp', flip: false }
  ]
  const header = new TextEncoder().encode(
    JSON.stringify({
      textures: textures.map((t) => ({ name: t.name, w: t.canvas.width, h: t.canvas.height, wrap: t.wrap })),
      props: props.length / FLOATS_PER_VERTEX,
      poses: poses.map((p) => p.length / FLOATS_PER_VERTEX),
      labels: atlas.labels,
      look: nativeLook(look)
    })
  )
  const blocks: Uint8Array[] = [...textures.map((t) => pixels(t.canvas, t.flip)), new Uint8Array(new Float32Array(props).buffer), ...poses.map((p) => new Uint8Array(new Float32Array(p).buffer))]
  const pad = (n: number): number => (n + 3) & ~3
  let total = pad(4 + header.length)
  for (const b of blocks) total += pad(b.length)
  const out = new Uint8Array(total)
  new DataView(out.buffer).setUint32(0, header.length, true)
  out.set(header, 4)
  let at = pad(4 + header.length)
  for (const b of blocks) {
    out.set(b, at)
    at += pad(b.length)
  }
  return out
}
