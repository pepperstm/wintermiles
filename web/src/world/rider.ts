import * as THREE from 'three'

// A low-poly cyclist assembled from primitives. Local frame: +Z forward, +Y up,
// origin on the ground midway between the wheels.

const WHEEL_R = 0.34
const CRANK = 0.17
const THIGH = 0.43
const SHIN = 0.45

const UP = new THREE.Vector3(0, 1, 0)
const tmp = new THREE.Vector3()

/** Stretches a unit cylinder between two points. */
function place(mesh: THREE.Mesh, a: THREE.Vector3, b: THREE.Vector3): void {
  tmp.subVectors(b, a)
  const len = tmp.length()
  mesh.position.addVectors(a, b).multiplyScalar(0.5)
  mesh.scale.set(1, len, 1)
  mesh.quaternion.setFromUnitVectors(UP, tmp.divideScalar(len || 1))
}

export class RiderModel {
  readonly group = new THREE.Group()
  private wheels: THREE.Group[] = []
  private crankPhase = 0
  private legs: { side: number; thigh: THREE.Mesh; shin: THREE.Mesh; crank: THREE.Mesh; foot: THREE.Mesh }[] = []
  private materials: THREE.Material[] = []
  private geometries: THREE.BufferGeometry[] = []

  constructor(jersey: number, ghost = false) {
    const mat = (color: number): THREE.MeshLambertMaterial => {
      const m = new THREE.MeshLambertMaterial({ color, transparent: ghost, opacity: ghost ? 0.38 : 1, depthWrite: !ghost })
      this.materials.push(m)
      return m
    }
    const geo = <T extends THREE.BufferGeometry>(g: T): T => {
      this.geometries.push(g)
      return g
    }
    const frameMat = mat(ghost ? 0xffffff : 0x22262e)
    const tyreMat = mat(ghost ? 0xffffff : 0x15171a)
    const jerseyMat = mat(jersey)
    const shortsMat = mat(ghost ? 0xffffff : 0x1b1d24)
    const skinMat = mat(ghost ? 0xffffff : 0xd9a47c)
    const helmetMat = mat(ghost ? 0xffffff : 0xf2f2f2)

    const tubeGeo = geo(new THREE.CylinderGeometry(1, 1, 1, 6))
    const tube = (a: THREE.Vector3, b: THREE.Vector3, r: number, m: THREE.Material): THREE.Mesh => {
      const holder = new THREE.Mesh(tubeGeo, m)
      place(holder, a, b)
      holder.scale.x = holder.scale.z = r
      this.group.add(holder)
      return holder
    }
    const v = (y: number, z: number, x = 0): THREE.Vector3 => new THREE.Vector3(x, y, z)

    const rearAxle = v(WHEEL_R, -0.5)
    const frontAxle = v(WHEEL_R, 0.5)
    const bb = v(0.29, -0.08)
    const seat = v(0.97, -0.24)
    const headTop = v(0.93, 0.36)
    const headBottom = v(0.78, 0.4)
    const bars = v(0.98, 0.46)
    const hips = v(1.02, -0.2)
    const shoulders = v(1.36, 0.22)

    // Wheels: a tyre plus two crossed spokes so their spin is visible.
    const tyreGeo = geo(new THREE.TorusGeometry(WHEEL_R - 0.02, 0.022, 6, 20))
    const spokeGeo = geo(new THREE.BoxGeometry(0.012, WHEEL_R * 2 - 0.06, 0.012))
    for (const axle of [rearAxle, frontAxle]) {
      const wheel = new THREE.Group()
      const tyre = new THREE.Mesh(tyreGeo, tyreMat)
      tyre.rotation.y = Math.PI / 2
      wheel.add(tyre)
      for (let i = 0; i < 2; i++) {
        const spoke = new THREE.Mesh(spokeGeo, frameMat)
        spoke.rotation.x = (i * Math.PI) / 2
        wheel.add(spoke)
      }
      wheel.position.copy(axle)
      this.group.add(wheel)
      this.wheels.push(wheel)
    }

    // Frame
    tube(bb, seat, 0.02, frameMat)
    tube(seat, headTop, 0.02, frameMat)
    tube(bb, headBottom, 0.024, frameMat)
    tube(rearAxle, bb, 0.014, frameMat)
    tube(rearAxle, seat, 0.012, frameMat)
    tube(headTop, frontAxle, 0.018, frameMat)
    tube(headTop, bars, 0.014, frameMat)
    tube(v(0.98, 0.46, -0.2), v(0.98, 0.46, 0.2), 0.012, frameMat)
    tube(v(0.99, -0.34), v(0.99, -0.12), 0.035, tyreMat) // saddle

    // Rider
    tube(hips, shoulders, 0.12, jerseyMat)
    tube(v(1.0, -0.26), v(1.06, -0.12), 0.13, shortsMat)
    for (const side of [-1, 1]) {
      tube(v(1.34, 0.2, side * 0.16), v(1.0, 0.45, side * 0.19), 0.035, skinMat)
    }
    const head = new THREE.Mesh(geo(new THREE.SphereGeometry(0.1, 10, 8)), skinMat)
    head.position.set(0, 1.52, 0.33)
    const helmet = new THREE.Mesh(geo(new THREE.SphereGeometry(0.115, 10, 6, 0, Math.PI * 2, 0, Math.PI * 0.55)), helmetMat)
    helmet.position.set(0, 1.535, 0.32)
    helmet.rotation.x = 0.25
    helmet.scale.z = 1.25
    this.group.add(head, helmet)

    for (const side of [-1, 1]) {
      this.legs.push({
        side,
        thigh: tube(hips, bb, 0.065, shortsMat),
        shin: tube(hips, bb, 0.045, skinMat),
        crank: tube(hips, bb, 0.012, frameMat),
        foot: tube(hips, bb, 0.035, helmetMat)
      })
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
    const bb = new THREE.Vector3(0, 0.29, -0.08)
    const hip = new THREE.Vector3()
    const pedal = new THREE.Vector3()
    const knee = new THREE.Vector3()
    const toe = new THREE.Vector3()
    for (const leg of this.legs) {
      const x = leg.side * 0.1
      const phase = this.crankPhase + (leg.side > 0 ? Math.PI : 0)
      // Forward pedalling: the top of the stroke moves toward the front wheel.
      pedal.set(x, bb.y + Math.cos(phase) * CRANK, bb.z + Math.sin(phase) * CRANK)
      hip.set(x, 1.0, -0.2)
      // Two-bone IK in the vertical plane, knee bending forward.
      const dy = pedal.y + 0.06 - hip.y
      const dz = pedal.z - hip.z
      const d = Math.min(THIGH + SHIN - 0.005, Math.hypot(dy, dz))
      const along = (THIGH * THIGH - SHIN * SHIN + d * d) / (2 * d)
      const off = Math.sqrt(Math.max(0, THIGH * THIGH - along * along))
      const uy = dy / d
      const uz = dz / d
      knee.set(x, hip.y + uy * along - uz * off, hip.z + uz * along + uy * off)
      // Choose the solution with the knee in front of the hip–pedal line.
      if (knee.z < hip.z + uz * along) knee.set(x, hip.y + uy * along + uz * off, hip.z + uz * along - uy * off)
      toe.set(x, pedal.y + 0.06, pedal.z)
      place(leg.thigh, hip, knee)
      leg.thigh.scale.x = leg.thigh.scale.z = 0.065
      place(leg.shin, knee, toe)
      leg.shin.scale.x = leg.shin.scale.z = 0.045
      bb.x = x
      place(leg.crank, bb, pedal)
      leg.crank.scale.x = leg.crank.scale.z = 0.012
      toe.set(x, pedal.y + 0.03, pedal.z - 0.09)
      knee.set(x, pedal.y + 0.03, pedal.z + 0.13)
      place(leg.foot, toe, knee)
      leg.foot.scale.x = leg.foot.scale.z = 0.035
    }
  }

  dispose(): void {
    this.materials.forEach((m) => m.dispose())
    this.geometries.forEach((g) => g.dispose())
  }
}
