import { useCallback, useEffect, useRef, useState } from 'react'
import { errorText } from '../bridge'
import { distUnit, distValue, fmtDist, fmtEle, fmtTime, gradeColor, speedUnit, speedValue, zoneColor } from '../core/fmt'
import type { Route } from '../core/gpx'
import { api, smokeHooks, useSettings } from '../state'
import type { RideInit, RideState, RideSummary } from '../types'
import { World } from '../world/scene'
import { MiniMap } from './MiniMap'
import { ProfileChart } from './ProfileChart'

interface Props {
  routeId: string
  onExit: () => void
  onDevices: () => void
  onFinish: (ride: RideSummary) => void
}

/** Rides shorter than this many recorded seconds are not saved (enforced by the backend). */
const MIN_SAMPLES = 60

const hex = (c: number): string => `#${c.toString(16).padStart(6, '0')}`

function fmtGap(seconds: number): string {
  const s = Math.abs(seconds)
  return `${seconds <= 0 ? '−' : '+'}${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`
}

export function Ride({ routeId, onExit, onDevices, onFinish }: Props): React.JSX.Element {
  const { settings, update } = useSettings()
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const worldRef = useRef<World | null>(null)
  const settingsRef = useRef(settings)
  settingsRef.current = settings
  // The world must not be rebuilt when the parent re-renders, so the effect below
  // reaches the latest callbacks through a ref instead of depending on them.
  const callbacks = useRef({ onExit, onFinish })
  callbacks.current = { onExit, onFinish }
  const [route, setRoute] = useState<Route | null>(null)
  const [init, setInit] = useState<RideInit | null>(null)
  const [loading, setLoading] = useState('Loading route…')
  const [error, setError] = useState('')
  const [hud, setHud] = useState<RideState | null>(null)
  const [saving, setSaving] = useState(false)
  const finishing = useRef(false)
  const units = settings.units

  const finish = useCallback(async (): Promise<void> => {
    if (finishing.current) return
    finishing.current = true
    setSaving(true)
    try {
      const summary = await api.ride.finish()
      if (summary) callbacks.current.onFinish(summary)
      else callbacks.current.onExit()
    } catch (err) {
      finishing.current = false
      setSaving(false)
      setError(`Could not save the ride: ${errorText(err)}`)
    }
  }, [])

  // Start the ride session in the backend, build the 3D view, then render until unmount.
  // The simulation itself runs in Rust; this loop only draws its latest state.
  useEffect(() => {
    let disposed = false
    let raf = 0
    let observer: ResizeObserver | null = null
    let offState: (() => void) | null = null
    const offLoading = api.ride.onLoading(setLoading)

    void (async () => {
      try {
        const started = await api.ride.start(routeId)
        if (disposed) return void api.ride.abort()
        const data = await api.ride.routeData()
        if (disposed) return
        const n = started.n
        const block = (i: number): Float64Array => new Float64Array(data, i * n * 8, n)
        const loaded: Route = {
          name: started.name,
          step: started.step,
          n,
          total: started.total,
          x: block(0),
          z: block(1),
          lat: block(2),
          lon: block(3),
          ele: block(4),
          grade: block(5),
          climbs: started.climbs
        }
        const canvas = canvasRef.current!
        const imperial = settingsRef.current.units === 'imperial'
        const world = new World(canvas, loaded, started.layout, started.covered, (i) => `${i} ${imperial ? 'mi' : 'km'}`, imperial ? 1609.344 : 1000)
        worldRef.current = world
        await world.load()
        if (disposed) return
        observer = new ResizeObserver(() => world.resize(canvas.clientWidth, canvas.clientHeight))
        observer.observe(canvas)
        world.resize(canvas.clientWidth, canvas.clientHeight)
        smokeHooks.capture = () => world.capture()
        smokeHooks.world = { realTerrain: started.realTerrain, buildings: started.buildings, scenery: started.scenery, covered: started.covered }
        const smoke = await api.smoke.config()
        for (let i = 0; i < (smoke?.camera ?? 0); i++) world.cycleCamera()

        const rivalInfo = new Map(started.rivals.map((r) => [r.id, r]))
        let latest: { s: RideState; at: number } | null = null
        let received = 0
        offState = api.ride.onState((s) => {
          latest = { s, at: performance.now() }
          smokeHooks.state = s
          // The simulation reports 20 times a second; the HUD only needs 5.
          if (received++ % 4 === 0) setHud(s)
          if (s.finished) void finish()
        })
        setRoute(loaded)
        setInit(started)
        setLoading('')

        let last = performance.now()
        const loop = (now: number): void => {
          raf = requestAnimationFrame(loop)
          const dt = Math.min(0.1, (now - last) / 1000)
          last = now
          if (!latest) return
          const s = latest.s
          // Glide between state updates so motion is smooth at any display refresh rate.
          const ahead = s.moving ? Math.min(0.25, (now - latest.at) / 1000) : 0
          world.frame(dt, {
            distance: Math.min(loaded.total, s.distance + s.speed * ahead),
            speed: s.speed,
            cadence: s.cadence ?? (s.moving && s.power > 20 ? 80 : 0),
            rivals: s.rivals.map((r) => {
              const info = rivalInfo.get(r.id)
              return {
                id: r.id,
                name: info?.name ?? '',
                color: info?.color ?? 0xffffff,
                ghost: info?.kind === 'ghost',
                distance: Math.min(loaded.total, r.distance + r.speed * ahead),
                speed: r.speed
              }
            })
          })
        }
        raf = requestAnimationFrame(loop)
      } catch (err) {
        if (!disposed) setError(errorText(err))
      }
    })()

    return () => {
      disposed = true
      cancelAnimationFrame(raf)
      observer?.disconnect()
      offState?.()
      offLoading()
      smokeHooks.capture = null
      worldRef.current?.dispose()
      worldRef.current = null
      // Harmless if the ride was already finished and saved.
      void api.ride.abort()
    }
  }, [routeId, finish])

  const togglePause = (): void => void api.ride.togglePause()
  const setTrainer = (patch: Partial<typeof settings.trainer>): void => update((s) => ({ ...s, trainer: { ...s.trainer, ...patch } }))
  const nudge = (delta: number): void => {
    const cfg = settingsRef.current.trainer
    if (cfg.mode === 'erg') setTrainer({ ergWatts: Math.max(50, Math.min(1000, cfg.ergWatts + delta)) })
    else void api.devices.demoNudge(delta)
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === ' ') togglePause()
      else if (e.key === 'c' || e.key === 'C') worldRef.current?.cycleCamera()
      else if (e.key === 'ArrowUp') nudge(10)
      else if (e.key === 'ArrowDown') nudge(-10)
      else return
      e.preventDefault()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  if (error) {
    return (
      <div className="ride-overlay solid">
        <div className="modal">
          <h2>Something went wrong</h2>
          <p>{error}</p>
          <button className="primary" onClick={onExit}>
            Back to routes
          </button>
        </div>
      </div>
    )
  }

  const ready = route && init && hud
  const climb = ready && hud.climbIndex !== null ? route.climbs[hud.climbIndex] : null
  const climbBest = ready && hud.climbIndex !== null ? init.pbClimbs[hud.climbIndex] : undefined
  const mode = settings.trainer.mode
  const standings = ready
    ? [
        { id: 'me', name: settings.rider.name || 'You', color: '#ff5a36', delta: 0, me: true },
        ...hud.rivals.map((r) => {
          const info = init.rivals.find((i) => i.id === r.id)
          return { id: r.id, name: info?.name ?? r.id, color: hex(info?.color ?? 0xffffff), delta: r.distance - hud.distance, me: false }
        })
      ].sort((a, b) => b.delta - a.delta)
    : []

  return (
    <div className="ride">
      <canvas ref={canvasRef} className="world" />

      {loading && (
        <div className="ride-overlay solid">
          <div className="spinner" />
          <div>{loading}</div>
        </div>
      )}

      {ready && (
        <>
          <div className="hud top">
            <div className="hud-title">
              <b>{route.name}</b>
              <span>
                {fmtDist(hud.distance, units, 2)} / {fmtDist(route.total, units)} · {fmtDist(route.total - hud.distance, units)} to go
              </span>
            </div>
            <div className="hud-clock">{fmtTime(hud.elapsed)}</div>
            <div className="hud-grade" style={{ color: gradeColor(hud.grade) }}>
              <b>
                {hud.grade >= 0 ? '▲' : '▼'} {Math.abs(hud.grade * 100).toFixed(1)}%
              </b>
              <span>
                {fmtEle(hud.ele, units)} · ↗ {fmtEle(hud.ascent, units)}
              </span>
            </div>
          </div>

          <div className="hud left">
            <div className="tile big" style={{ borderColor: zoneColor(hud.power, settings.rider.ftp) }}>
              <div className="tile-value" style={{ color: zoneColor(hud.power, settings.rider.ftp) }}>
                {Math.round(hud.power)}
              </div>
              <div className="tile-label">
                watts · {(hud.power / settings.rider.weightKg).toFixed(1)} W/kg
              </div>
            </div>
            <div className="tile">
              <div className="tile-value">{speedValue(hud.speed, units).toFixed(1)}</div>
              <div className="tile-label">{speedUnit(units)}</div>
            </div>
            <div className="tile">
              <div className="tile-value">{hud.hr === null ? '–' : Math.round(hud.hr)}</div>
              <div className="tile-label">♥ bpm</div>
            </div>
            <div className="tile">
              <div className="tile-value">{hud.cadence === null ? '–' : Math.round(hud.cadence)}</div>
              <div className="tile-label">rpm</div>
            </div>
          </div>

          <div className="hud right">
            {hud.gap !== null && (
              <div className={hud.gap <= 0 ? 'gap ahead' : 'gap behind'}>
                {fmtGap(hud.gap)} <span>vs your best</span>
              </div>
            )}
            {standings.length > 1 && (
              <div className="standings">
                {standings.map((r) => (
                  <div key={r.id} className={r.me ? 'me' : ''}>
                    <i style={{ background: r.color }} />
                    <span>{r.name}</span>
                    <em>{r.me ? '' : `${r.delta >= 0 ? '+' : '−'}${Math.abs(r.delta) >= 1000 ? `${distValue(Math.abs(r.delta), units).toFixed(1)} ${distUnit(units)}` : `${Math.round(Math.abs(r.delta))} m`}`}</em>
                  </div>
                ))}
              </div>
            )}
            <MiniMap route={route} lat={hud.lat} lon={hud.lon} />
          </div>

          {climb && (
            <div className="banner climb">
              <b>
                Climb {hud.climbIndex! + 1}
                {climb.category && ` · Cat ${climb.category}`}
              </b>
              <span>
                {fmtDist(Math.max(0, climb.end - hud.distance), units, 2)} to the top · avg {(climb.avgGrade * 100).toFixed(1)}% · {fmtTime(hud.climbElapsed)}
                {climbBest !== undefined && ` (best ${fmtTime(climbBest)})`}
              </span>
              <div className="bar">
                <div style={{ width: `${Math.min(100, ((hud.distance - climb.start) / climb.length) * 100)}%` }} />
              </div>
            </div>
          )}
          {!climb && hud.climbResult && (
            <div className="banner summit">
              <b>
                Summit! Climb {hud.climbResult.index + 1} in {fmtTime(hud.climbResult.timeS)}
              </b>
              <span>
                {hud.climbResult.previousBest === null
                  ? 'First time up — that is the time to beat.'
                  : hud.climbResult.timeS < hud.climbResult.previousBest
                    ? `New best, ${fmtTime(hud.climbResult.previousBest - hud.climbResult.timeS)} faster!`
                    : `${fmtTime(hud.climbResult.timeS - hud.climbResult.previousBest)} off your best.`}
              </span>
            </div>
          )}

          {!hud.moving && !saving && !hud.finished && (
            <div className="ride-overlay">
              <div className="modal">
                {hud.noSensors ? (
                  <>
                    <h2>No sensors connected</h2>
                    <p className="muted">Connect a trainer or power meter, or try the route with a simulated rider.</p>
                    <div className="row">
                      <button className="primary" onClick={onDevices}>
                        Connect devices
                      </button>
                      <button onClick={() => void api.devices.demoAdd()}>Use demo rider</button>
                    </div>
                  </>
                ) : hud.paused ? (
                  <>
                    <h2>Paused</h2>
                    <button className="primary" onClick={togglePause}>
                      Resume
                    </button>
                  </>
                ) : (
                  <>
                    <h2>{hud.elapsed > 0 ? 'Stopped' : 'Start pedalling'}</h2>
                    <p className="muted">
                      {hud.hasPower
                        ? 'The clock runs while you ride.'
                        : 'No power reading yet — wake your trainer or power meter. (Speed-sensor-only setups: enable power estimation in Settings.)'}
                    </p>
                  </>
                )}
              </div>
            </div>
          )}

          <div className="hud bottom">
            <div className="controls">
              <button onClick={togglePause}>{hud.paused ? 'Resume' : 'Pause'}</button>
              <button onClick={() => worldRef.current?.cycleCamera()}>Camera</button>
              <div className="seg">
                <button className={mode === 'sim' ? 'on' : ''} onClick={() => setTrainer({ mode: 'sim' })} title="Trainer resistance follows the gradient">
                  Gradient
                </button>
                <button className={mode === 'erg' ? 'on' : ''} onClick={() => setTrainer({ mode: 'erg' })} title="Trainer holds a fixed power">
                  ERG
                </button>
              </div>
              {mode === 'erg' ? (
                <div className="stepper">
                  <button onClick={() => nudge(-10)}>−</button>
                  <span>{settings.trainer.ergWatts} W</span>
                  <button onClick={() => nudge(10)}>+</button>
                </div>
              ) : (
                <div className="stepper" title="How much of the gradient the trainer applies">
                  <button onClick={() => setTrainer({ difficulty: Math.max(0, Math.round((settings.trainer.difficulty - 0.05) * 100) / 100) })}>−</button>
                  <span>Feel {Math.round(settings.trainer.difficulty * 100)}%</span>
                  <button onClick={() => setTrainer({ difficulty: Math.min(1, Math.round((settings.trainer.difficulty + 0.05) * 100) / 100) })}>+</button>
                </div>
              )}
              {hud.demoTarget !== null && mode === 'sim' && (
                <div className="stepper" title="Demo rider effort (↑/↓)">
                  <button onClick={() => nudge(-10)}>−</button>
                  <span>Demo {Math.round(hud.demoTarget)} W</span>
                  <button onClick={() => nudge(10)}>+</button>
                </div>
              )}
              {!hud.hasTrainer && !hud.noSensors && <span className="hint">No controllable trainer — resistance is manual</span>}
              <span className="spacer" />
              <button
                className="danger"
                disabled={saving}
                onClick={() => {
                  if (hud.samples >= MIN_SAMPLES) void finish()
                  else if (hud.samples === 0 || confirm('Rides under a minute are not saved. Leave anyway?')) onExit()
                }}
              >
                {saving ? 'Saving…' : 'End ride'}
              </button>
            </div>
            <div className="profiles">
              <div className="profile-next">
                <ProfileChart route={route} distance={hud.distance} from={hud.distance - 150} to={hud.distance + 1350} />
                <label>Next {fmtDist(1350, units, 1)}</label>
              </div>
              <div className="profile-all">
                <ProfileChart route={route} distance={hud.distance} markers={standings.filter((r) => !r.me).map((r) => ({ distance: hud.distance + r.delta, color: r.color }))} />
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  )
}
