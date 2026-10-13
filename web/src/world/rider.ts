import * as THREE from 'three'
import { mergeGeometries } from 'three/examples/jsm/utils/BufferGeometryUtils.js'

// A cyclist on a road bike, modelled from real proportions: a 56 cm frame with
// drop bars and 700c wheels, and a 1.78 m rider on the hoods. Local frame: +Z
// forward, +Y up, origin on the ground midway between the wheels; the drive
// side is therefore −X.
//
// Everything that doesn't move is baked into one mesh with vertex colours, so
// a rider costs a dozen draw calls however much detail is on it.

const WHEEL_R = 0.34
const CRANK = 0.1725
const THIGH = 0.44
const SHIN = 0.43
/** Half the distance between the feet, the hip joints and the shoulders. */
const FOOT_X = 0.125
const HIP_X = 0.085
const SHOULDER_X = 0.165

const UP = new THREE.Vector3(0, 1, 0)
const tmp = new THREE.Vector3()
const ONE = new THREE.Vector3(1, 1, 1)

/** A point given the way a bike is drawn: height, then distance forward, then sideways. */
const v = (y: number, z: number, x = 0): THREE.Vector3 => new THREE.Vector3(x, y, z)

// Frame geometry.
const REAR_AXLE = v(WHEEL_R, -0.5)
const FRONT_AXLE = v(WHEEL_R, 0.5)
const BB = v(0.27, -0.096)
const SEAT_CLUSTER = v(0.77, -0.244)
const SADDLE = v(0.972, -0.3)
const HEAD_TOP = v(0.845, 0.291)
const HEAD_BOTTOM = v(0.68, 0.3415)
const STEM_CLAMP = v(0.875, 0.385)
const HOOD = v(0.925, 0.47)

// The rider.
const HIP = v(1.045, -0.265)
const SHOULDER = v(1.365, 0.13)
const HEAD = v(1.535, 0.265)
/** Where the ankle is, seen from the ball of the foot with the shoe level. */
const ANKLE = { up: 0.085, back: 0.125 }

const COLOURS = {
  frame: 0x23272f,
  metal: 0x9aa1ab,
  rubber: 0x141619,
  rim: 0x2b2e35,
  shorts: 0x181a20,
  skin: 0xd9a47c,
  hair: 0x3a2a1e,
  helmet: 0xf2f2f2,
  sock: 0xf4f4f4,
  shoe: 0xe9e9e9,
  glasses: 0x1a1d22,
  bottle: 0xdfe6ee
}

function paint<T extends THREE.BufferGeometry>(g: T, colour: number): T {
  const c = new THREE.Color(colour)
  const n = g.getAttribute('position').count
  const colours = new Float32Array(n * 3)
  for (let i = 0; i < n; i++) colours.set([c.r, c.g, c.b], i * 3)
  g.setAttribute('color', new THREE.BufferAttribute(colours, 3))
  return g
}

/**
 * Part of a limb modelled along +Y from −½ to +½: the stretch between two
 * fractions of its length, tapering from `from` (radius at the start of the
 * whole limb) to `to` (at its end).
 */
function taper(from: number, to: number, t0 = 0, t1 = 1, sides = 10): THREE.BufferGeometry {
  const r = (t: number): number => from + (to - from) * t
  return new THREE.CylinderGeometry(r(t1), r(t0), t1 - t0, sides).translate(0, (t0 + t1) / 2 - 0.5, 0)
}

/** The transform that stretches a limb modelled along +Y between two points, optionally flattened. */
function between(a: THREE.Vector3, b: THREE.Vector3, wide = 1, deep = 1): THREE.Matrix4 {
  tmp.subVectors(b, a)
  const len = tmp.length()
  const turn = new THREE.Quaternion().setFromUnitVectors(UP, tmp.clone().divideScalar(len || 1))
  return new THREE.Matrix4().compose(a.clone().add(b).multiplyScalar(0.5), turn, new THREE.Vector3(wide, len, deep))
}

/** Moves a limb to run from `a` to `b`. */
function place(mesh: THREE.Object3D, a: THREE.Vector3, b: THREE.Vector3): void {
  tmp.subVectors(b, a)
  const len = tmp.length()
  mesh.position.addVectors(a, b).multiplyScalar(0.5)
  mesh.scale.set(1, len, 1)
  mesh.quaternion.setFromUnitVectors(UP, tmp.divideScalar(len || 1))
}

/**
 * The middle joint of a two-bone limb lying in the plane of travel: of the two
 * places it could be, the one further toward `forward` (+1) or back (−1).
 */
function joint(a: THREE.Vector3, b: THREE.Vector3, first: number, second: number, forward: number, out: THREE.Vector3): THREE.Vector3 {
  const dy = b.y - a.y
  const dz = b.z - a.z
  const d = Math.min(first + second - 0.004, Math.hypot(dy, dz))
  const along = (first * first - second * second + d * d) / (2 * d)
  const off = Math.sqrt(Math.max(0, first * first - along * along))
  const uy = dy / d
  const uz = dz / d
  const side = Math.sign(uy * forward) || 1
  return out.set((a.x + b.x) / 2, a.y + uy * along - uz * off * side, a.z + uz * along + uy * off * side)
}

/** Gathers shapes, each moved into place and coloured, to be baked into one mesh. */
class Parts {
  private list: THREE.BufferGeometry[] = []

  add(g: THREE.BufferGeometry, colour: number, at?: THREE.Matrix4): this {
    if (at) g.applyMatrix4(at)
    this.list.push(paint(g, colour))
    return this
  }

  /** A round tube from `a` to `b`. */
  tube(a: THREE.Vector3, b: THREE.Vector3, r: number, colour: number, sides = 8): this {
    return this.add(taper(r, r, 0, 1, sides), colour, between(a, b))
  }

  /** A limb or body segment from `a` to `b`, tapering, with an oval section `wide` by `deep` times its radii. */
  limb(a: THREE.Vector3, b: THREE.Vector3, from: number, to: number, colour: number, wide = 1, deep = 1): this {
    return this.add(taper(from, to, 0, 1, 12), colour, between(a, b, wide, deep))
  }

  ball(at: THREE.Vector3, r: number, colour: number, scale = ONE): this {
    return this.add(new THREE.SphereGeometry(r, 12, 9), colour, new THREE.Matrix4().compose(at, new THREE.Quaternion(), scale))
  }

  bake(): THREE.BufferGeometry {
    const merged = mergeGeometries(this.list)!
    this.list.forEach((g) => g.dispose())
    return merged
  }
}

function wheelGeometry(): THREE.BufferGeometry {
  const parts = new Parts()
  parts.add(new THREE.TorusGeometry(WHEEL_R - 0.014, 0.014, 8, 36).rotateY(Math.PI / 2), COLOURS.rubber)
  // A mid-depth rim: deeper toward the hub than it is wide.
  parts.add(new THREE.TorusGeometry(WHEEL_R - 0.046, 0.02, 6, 36).scale(1, 1, 0.55).rotateY(Math.PI / 2), COLOURS.rim)
  const spokes = 14
  for (let i = 0; i < spokes; i++) {
    parts.add(new THREE.BoxGeometry(0.003, WHEEL_R - 0.05, 0.003).translate(i % 2 ? 0.012 : -0.012, (WHEEL_R - 0.05) / 2, 0).rotateX((i / spokes) * Math.PI * 2), COLOURS.metal)
  }
  parts.add(new THREE.CylinderGeometry(0.019, 0.019, 0.1, 8).rotateZ(Math.PI / 2), COLOURS.metal)
  return parts.bake()
}

/** Chainring and both crank arms, modelled with the drive-side arm pointing up. */
function cranksetGeometry(): THREE.BufferGeometry {
  const parts = new Parts()
  parts.add(new THREE.CylinderGeometry(0.098, 0.098, 0.005, 28).rotateZ(Math.PI / 2).translate(-0.05, 0, 0), COLOURS.rim)
  parts.add(new THREE.CylinderGeometry(0.03, 0.03, 0.012, 12).rotateZ(Math.PI / 2).translate(-0.056, 0, 0), COLOURS.metal)
  parts.add(new THREE.CylinderGeometry(0.014, 0.014, 0.15, 8).rotateZ(Math.PI / 2), COLOURS.frame)
  parts.add(new THREE.BoxGeometry(0.014, CRANK + 0.03, 0.028).translate(-0.068, CRANK / 2, 0), COLOURS.frame)
  parts.add(new THREE.BoxGeometry(0.014, CRANK + 0.03, 0.028).translate(0.068, -CRANK / 2, 0), COLOURS.frame)
  return parts.bake()
}

/** A shoe on its pedal, modelled about the pedal axle with the toe toward +Z. */
function footGeometry(side: number): THREE.BufferGeometry {
  const parts = new Parts()
  parts.ball(v(0.043, -0.045), 1, COLOURS.shoe, new THREE.Vector3(0.047, 0.04, 0.14))
  parts.ball(v(0.012, -0.05), 1, COLOURS.rubber, new THREE.Vector3(0.042, 0.012, 0.125)) // sole
  parts.ball(v(ANKLE.up, -ANKLE.back), 0.04, COLOURS.sock)
  parts.add(new THREE.BoxGeometry(0.095, 0.016, 0.07).translate(-side * 0.015, -0.006, 0), COLOURS.frame) // pedal
  return parts.bake()
}

function bikeAndBody(parts: Parts, jersey: number): void {
  const C = COLOURS

  // ── Frame ──
  parts.tube(BB, SEAT_CLUSTER, 0.017, C.frame)
  parts.tube(SEAT_CLUSTER, v(0.822, 0.297), 0.016, C.frame) // top tube
  parts.tube(BB, v(0.705, 0.334), 0.023, C.frame) // down tube
  parts.tube(HEAD_BOTTOM, HEAD_TOP, 0.022, C.frame)
  parts.tube(SEAT_CLUSTER, v(0.955, -0.299), 0.0135, C.rubber) // seatpost
  parts.add(new THREE.CylinderGeometry(0.024, 0.024, 0.075, 10).rotateZ(Math.PI / 2).translate(0, BB.y, BB.z), C.frame)
  for (const side of [-1, 1]) {
    parts.tube(v(BB.y, BB.z + 0.01, side * 0.028), v(REAR_AXLE.y, REAR_AXLE.z, side * 0.064), 0.01, C.frame) // chainstay
    parts.tube(v(SEAT_CLUSTER.y - 0.03, SEAT_CLUSTER.z, side * 0.02), v(REAR_AXLE.y, REAR_AXLE.z, side * 0.064), 0.008, C.frame) // seatstay
    parts.tube(v(HEAD_BOTTOM.y + 0.005, HEAD_BOTTOM.z, side * 0.03), v(FRONT_AXLE.y, FRONT_AXLE.z, side * 0.052), 0.011, C.frame) // fork blade
  }

  // ── Cockpit: stem, drop bars with their tape, brake hoods and levers ──
  const steerer = v(0.868, 0.284)
  parts.tube(HEAD_TOP, steerer, 0.017, C.rubber)
  parts.tube(steerer, STEM_CLAMP, 0.015, C.rubber)
  parts.tube(v(STEM_CLAMP.y, STEM_CLAMP.z, -0.2), v(STEM_CLAMP.y, STEM_CLAMP.z, 0.2), 0.0125, C.rubber)
  for (const side of [-1, 1]) {
    const x = side * 0.2
    const bend = [v(STEM_CLAMP.y, STEM_CLAMP.z, x), v(0.88, 0.45, x), v(0.845, 0.485, x), v(0.785, 0.485, x), v(0.74, 0.445, x), v(0.73, 0.365, x)]
    for (let i = 0; i + 1 < bend.length; i++) {
      parts.tube(bend[i], bend[i + 1], 0.0125, C.rubber)
      parts.ball(bend[i + 1], 0.0125, C.rubber)
    }
    parts.limb(v(0.88, 0.45, x), v(0.94, 0.492, x), 0.018, 0.013, C.rubber) // hood
    parts.tube(v(0.925, 0.497, x), v(0.795, 0.515, x), 0.005, C.metal, 6) // brake lever
  }

  // ── Saddle, bottle, and the parts of the drivetrain that stay put ──
  parts.ball(SADDLE, 1, C.rubber, new THREE.Vector3(0.068, 0.018, 0.1))
  parts.ball(v(SADDLE.y - 0.003, SADDLE.z + 0.08), 1, C.rubber, new THREE.Vector3(0.028, 0.016, 0.07))
  parts.tube(v(0.44, 0.03), v(0.6, 0.142), 0.034, C.bottle, 10)
  parts.tube(v(0.6, 0.142), v(0.635, 0.167), 0.02, jersey, 8)
  parts.add(new THREE.CylinderGeometry(0.045, 0.028, 0.03, 12).rotateZ(Math.PI / 2).translate(-0.045, REAR_AXLE.y, REAR_AXLE.z), C.metal) // cassette
  const pulley = v(0.215, -0.475, -0.05)
  parts.tube(v(REAR_AXLE.y - 0.02, REAR_AXLE.z, -0.055), pulley, 0.009, C.frame, 6) // rear derailleur
  parts.tube(v(BB.y + 0.097, BB.z, -0.05), v(REAR_AXLE.y + 0.042, REAR_AXLE.z, -0.05), 0.0045, C.rim, 5) // chain, upper run
  parts.tube(v(BB.y - 0.097, BB.z, -0.05), pulley, 0.0045, C.rim, 5) // chain, lower run

  // ── Rider: a back that curves from the saddle up to the shoulders ──
  const lean = SHOULDER.clone().sub(HIP).normalize()
  const arch = new THREE.Vector3(0, lean.z, -lean.y)
  const waist = HIP.clone().addScaledVector(lean, 0.17).addScaledVector(arch, 0.035)
  const chest = HIP.clone().addScaledVector(lean, 0.35).addScaledVector(arch, 0.04)
  parts.ball(HIP, 1, C.shorts, new THREE.Vector3(0.165, 0.115, 0.135))
  parts.limb(HIP, waist, 1, 0.94, jersey, 0.155, 0.11)
  parts.ball(waist, 1, jersey, new THREE.Vector3(0.147, 0.105, 0.105))
  parts.limb(waist, chest, 0.86, 1, jersey, 0.172, 0.122)
  parts.ball(chest, 1, jersey, new THREE.Vector3(0.173, 0.123, 0.123))
  parts.limb(chest, SHOULDER, 1, 0.9, jersey, 0.172, 0.118)
  parts.ball(SHOULDER, 1, jersey, new THREE.Vector3(0.156, 0.09, 0.1))

  // Arms, a little bent, elbows out, hands on the hoods.
  for (const side of [-1, 1]) {
    const shoulder = v(SHOULDER.y - 0.015, SHOULDER.z, side * SHOULDER_X)
    const hand = v(HOOD.y + 0.012, HOOD.z - 0.005, side * 0.2)
    const elbow = joint(shoulder, hand, 0.3, 0.275, -1, new THREE.Vector3())
    elbow.x = side * (SHOULDER_X + 0.045)
    const cuff = shoulder.clone().lerp(elbow, 0.55)
    parts.ball(shoulder, 0.056, jersey)
    parts.limb(shoulder, cuff, 0.054, 0.047, jersey)
    parts.limb(cuff, elbow, 0.043, 0.037, C.skin)
    parts.ball(elbow, 0.038, C.skin)
    parts.limb(elbow, hand, 0.037, 0.028, C.skin)
    parts.ball(hand, 1, C.rubber, new THREE.Vector3(0.036, 0.03, 0.05)) // glove
  }

  // Neck and head, looking up the road; hair at the nape, glasses, and a helmet.
  parts.limb(v(SHOULDER.y + 0.01, SHOULDER.z + 0.02), HEAD, 0.05, 0.043, C.skin)
  parts.ball(v(HEAD.y - 0.012, HEAD.z - 0.04), 1, C.hair, new THREE.Vector3(0.08, 0.085, 0.075))
  parts.ball(HEAD, 1, C.skin, new THREE.Vector3(0.086, 0.106, 0.098))
  parts.ball(v(HEAD.y - 0.065, HEAD.z + 0.04), 1, C.skin, new THREE.Vector3(0.062, 0.05, 0.062)) // jaw
  parts.ball(v(HEAD.y - 0.022, HEAD.z + 0.1), 0.015, C.skin) // nose
  parts.add(new THREE.BoxGeometry(0.164, 0.04, 0.05).translate(0, HEAD.y + 0.006, HEAD.z + 0.076), C.glasses)
  const helmet = new THREE.Matrix4().compose(v(HEAD.y + 0.036, HEAD.z - 0.012), new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(1, 0, 0), 0.1), new THREE.Vector3(0.96, 0.9, 1.26))
  parts.add(new THREE.SphereGeometry(0.108, 14, 8, 0, Math.PI * 2, 0, Math.PI * 0.5), C.helmet, helmet)
  parts.add(new THREE.CylinderGeometry(0.108, 0.104, 0.012, 14), C.glasses, helmet) // the shell's rim
}

let disc: THREE.CanvasTexture | null = null

/** A blurred blob, dark in the middle and fading to nothing: shared by every rider's shadow. */
function softDisc(): THREE.CanvasTexture {
  if (disc) return disc
  const canvas = document.createElement('canvas')
  canvas.width = canvas.height = 64
  const ctx = canvas.getContext('2d')!
  const fade = ctx.createRadialGradient(32, 32, 0, 32, 32, 32)
  fade.addColorStop(0, '#ffffff')
  fade.addColorStop(0.45, '#9a9a9a')
  fade.addColorStop(1, '#000000')
  ctx.fillStyle = fade
  ctx.fillRect(0, 0, 64, 64)
  disc = new THREE.CanvasTexture(canvas)
  return disc
}

interface Leg {
  side: number
  thigh: THREE.Mesh
  knee: THREE.Mesh
  shin: THREE.Mesh
  foot: THREE.Mesh
}

export class RiderModel {
  readonly group = new THREE.Group()
  private wheels: THREE.Mesh[] = []
  private crankset: THREE.Mesh
  private crankPhase = 0
  private legs: Leg[] = []
  private materials: THREE.Material[] = []
  private geometries: THREE.BufferGeometry[] = []

  constructor(jersey: number, ghost = false) {
    // A ghost is the same figure in translucent white.
    const material = ghost ? new THREE.MeshLambertMaterial({ color: 0xffffff, transparent: true, opacity: 0.38, depthWrite: false }) : new THREE.MeshLambertMaterial({ vertexColors: true })
    this.materials.push(material)
    const mesh = (g: THREE.BufferGeometry): THREE.Mesh => {
      this.geometries.push(g)
      const m = new THREE.Mesh(g, material)
      this.group.add(m)
      return m
    }

    const fixed = new Parts()
    bikeAndBody(fixed, jersey)
    mesh(fixed.bake())

    const wheel = wheelGeometry()
    for (const axle of [REAR_AXLE, FRONT_AXLE]) {
      const w = mesh(wheel)
      w.position.copy(axle)
      this.wheels.push(w)
    }
    this.crankset = mesh(cranksetGeometry())
    this.crankset.position.copy(BB)

    // Legs: shorts to just above the knee, a sock above the shoe.
    const thigh = new Parts().add(taper(0.082, 0.054, 0, 0.8, 12), COLOURS.shorts).add(taper(0.082, 0.054, 0.8, 1, 12), COLOURS.skin).bake()
    const shin = new Parts().add(taper(0.052, 0.033, 0, 0.72, 12), COLOURS.skin).add(taper(0.052, 0.033, 0.72, 1, 12), COLOURS.sock).bake()
    const knee = paint(new THREE.SphereGeometry(0.054, 12, 9), COLOURS.skin)
    for (const side of [-1, 1]) {
      this.legs.push({ side, thigh: mesh(thigh), knee: mesh(knee), shin: mesh(shin), foot: mesh(footGeometry(side)) })
    }

    if (!ghost) {
      // A soft shadow on the road, which is most of what makes the bike look planted on it.
      const shade = new THREE.MeshBasicMaterial({ color: 0x000000, alphaMap: softDisc(), transparent: true, opacity: 0.42, depthWrite: false, polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -4 })
      this.materials.push(shade)
      const shadow = new THREE.Mesh(new THREE.PlaneGeometry(2, 2).rotateX(-Math.PI / 2).scale(0.34, 1, 0.95), shade)
      this.geometries.push(shadow.geometry)
      shadow.position.y = 0.02
      this.group.add(shadow)
    }
    this.pose()
  }

  /** Advances wheel spin and pedalling for this frame. */
  update(dt: number, speed: number, cadence: number): void {
    const spin = (speed / WHEEL_R) * dt
    for (const w of this.wheels) w.rotation.x += spin
    if (cadence > 0) {
      this.crankPhase += (cadence / 60) * Math.PI * 2 * dt
      this.pose()
    }
  }

  private pose(): void {
    // Forward pedalling: the top of the stroke moves toward the front wheel.
    this.crankset.rotation.x = this.crankPhase
    const hip = new THREE.Vector3()
    const ankle = new THREE.Vector3()
    const knee = new THREE.Vector3()
    for (const leg of this.legs) {
      const phase = this.crankPhase + (leg.side > 0 ? Math.PI : 0)
      const pedalY = BB.y + Math.cos(phase) * CRANK
      const pedalZ = BB.z + Math.sin(phase) * CRANK
      // The foot is nearly level over the top of the stroke and points down through the bottom.
      const pitch = 0.22 - 0.12 * Math.cos(phase - 0.5)
      leg.foot.position.set(leg.side * FOOT_X, pedalY, pedalZ)
      leg.foot.rotation.x = pitch
      ankle.set(leg.side * FOOT_X, pedalY + ANKLE.up * Math.cos(pitch) + ANKLE.back * Math.sin(pitch), pedalZ + ANKLE.up * Math.sin(pitch) - ANKLE.back * Math.cos(pitch))
      hip.set(leg.side * HIP_X, HIP.y, HIP.z)
      joint(hip, ankle, THIGH, SHIN, 1, knee)
      place(leg.thigh, hip, knee)
      place(leg.shin, knee, ankle)
      leg.knee.position.copy(knee)
    }
  }

  dispose(): void {
    this.materials.forEach((m) => m.dispose())
    this.geometries.forEach((g) => g.dispose())
  }
}
