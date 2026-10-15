import { useCallback, useEffect, useRef, useState } from 'react'
import { errorText } from '../bridge'
import { distUnit, distValue, fmtDist, fmtEle, fmtTime, gradeColor, speedUnit, speedValue, zoneColor } from '../core/fmt'
import type { Route } from '../core/gpx'
import { api, smokeHooks, useSettings } from '../state'
import type { RideInit, RideRequest, RideState, RideSummary } from '../types'
import { WorkoutShape } from './Workouts'
import { World } from '../world/scene'
import { look, timeFromHour, weatherFromCode } from '../world/weather'
import { MiniMap } from './MiniMap'
import { StreetPhoto } from './StreetPhoto'
import { ProfileChart } from './ProfileChart'

interface Props {
  request: RideRequest
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

/** Coggan zone number for a power. */
function zone(power: number, ftp: number): number {
  const r = ftp > 0 ? power / ftp : 0
  return [0.55, 0.75, 0.9, 1.05, 1.2, 1.5].filter((l) => r >= l).length + 1
}

export function Ride({ request, onExit, onDevices, onFinish }: Props): React.JSX.Element {
  const routeId = request.routeId
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
    let pump = 0
    let observer: ResizeObserver | null = null
    let offState: (() => void) | null = null
    const offLoading = api.ride.onLoading(setLoading)

    void (async () => {
      try {
        const started = await api.ride.start(request)
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
        const world = new World(canvas, loaded, started.layout, started.covered, (i) => `${i} ${imperial ? 'mi' : 'km'}`, imperial ? 1609.344 : 1000, started.landmarks)
        worldRef.current = world
        await world.load()
        if (disposed) return
        const lookCfg = settingsRef.current.look
        const time = lookCfg.timeOfDay === 'clock' ? timeFromHour(started.hour) : lookCfg.timeOfDay
        let weather = lookCfg.weather === 'live' ? 'clear' : lookCfg.weather
        if (lookCfg.weather === 'live') {
          // The weather there right now; if it can't be had, a clear day.
          weather = await api.weatherNow(started.centre[0], started.centre[1]).then(weatherFromCode, () => 'clear' as const)
        }
        world.setLook(look(time, weather))
        observer = new ResizeObserver(() => world.resize(canvas.clientWidth, canvas.clientHeight))
        observer.observe(canvas)
        world.resize(canvas.clientWidth, canvas.clientHeight)
        smokeHooks.capture = () => world.capture()
        smokeHooks.world = { realTerrain: started.realTerrain, buildings: started.buildings, scenery: started.scenery, covered: started.covered, frames: () => frames }
        const smoke = await api.smoke.config()
        for (let i = 0; i < (smoke?.camera ?? 0); i++) world.cycleCamera()

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
        let frames = 0
        let frameFailed = false
        const loop = (now: number): void => {
          raf = requestAnimationFrame(loop)
          const dt = Math.min(0.1, (now - last) / 1000)
          last = now
          if (!latest) return
          const s = latest.s
          // Glide between state updates so motion is smooth at any display refresh rate.
          const ahead = s.moving ? Math.min(0.25, (now - latest.at) / 1000) : 0
          try {
            world.frame(dt, {
              distance: Math.min(loaded.total, s.distance + s.speed * ahead),
              speed: s.speed,
              cadence: s.cadence ?? (s.moving && s.power > 20 ? 80 : 0),
              rivals: s.rivals.map((r) => ({
                id: r.id,
                name: r.name,
                color: r.color,
                ghost: r.kind === 'ghost',
                distance: Math.min(loaded.total, r.distance + r.speed * ahead),
                speed: r.speed
              }))
            })
            frames++
          } catch (err) {
            // Report a broken frame once, rather than failing silently sixty times a second.
            if (!frameFailed) void api.log(`frame failed: ${err instanceof Error ? (err.stack ?? err.message) : String(err)}`)
            frameFailed = true
          }
        }
        raf = requestAnimationFrame(loop)
        // A window that is hidden behind others gets no animation frames. The automated check must
        // still end up with a picture, so there it also draws on a timer.
        if (smoke) {
          pump = window.setInterval(() => {
            cancelAnimationFrame(raf)
            loop(performance.now())
          }, 100)
        }
      } catch (err) {
        if (!disposed) setError(errorText(err))
      }
    })()

    return () => {
      disposed = true
      cancelAnimationFrame(raf)
      window.clearInterval(pump)
      observer?.disconnect()
      offState?.()
      offLoading()
      smokeHooks.capture = null
      worldRef.current?.dispose()
      worldRef.current = null
      // Harmless if the ride was already finished and saved.
      void api.ride.abort()
    }
    // The request is fixed for the life of this screen.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [routeId, finish])

  const workoutRef = useRef(false)
  workoutRef.current = Boolean(init?.workout)
  const togglePause = (): void => void api.ride.togglePause()
  const setTrainer = (patch: Partial<typeof settings.trainer>): void => update((s) => ({ ...s, trainer: { ...s.trainer, ...patch } }))
  const nudge = (delta: number): void => {
    const cfg = settingsRef.current.trainer
    if (workoutRef.current) void api.ride.intensity(delta > 0 ? 0.05 : -0.05)
    else if (cfg.mode === 'erg') setTrainer({ ergWatts: Math.max(50, Math.min(1000, cfg.ergWatts + delta)) })
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
        ...hud.rivals.map((r) => ({ id: r.id, name: r.name, color: hex(r.color), delta: r.distance - hud.distance, me: false }))
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
              {init.landmarks
                .filter((m) => m.distance > hud.distance && m.distance < hud.distance + 5000)
                .slice(0, 3)
                .map((m, i) => (
                  <span key={i} className="coming-up">
                    {m.kind === 'summit' ? `⛰ ${m.name} ${m.detail}` : m.name} · {fmtDist(m.distance - hud.distance, units, 1)}
                  </span>
                ))}
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
                watts · {(hud.power / settings.rider.weightKg).toFixed(1)} W/kg · Z{zone(hud.power, settings.rider.ftp)}
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

          {init.workout && (
            <div className="hud workout-panel">
              {hud.workout ? (
                <>
                  <div className="wk-now">
                    <div>
                      <b style={{ color: zoneColor(hud.workoutTarget ?? 0, settings.rider.ftp) }}>
                        {hud.workout.kind === 'power' ? `${Math.round(hud.workoutTarget ?? 0)} W` : hud.workout.kind === 'max' ? 'ALL OUT' : 'Free ride'}
                      </b>
                      <span>
                        {hud.workout.kind === 'power' ? `${Math.round(hud.workout.target * 100 * hud.intensity)}% FTP` : 'no target'}
                        {hud.workout.cadence ? ` · ${hud.workout.cadence} rpm` : ''}
                      </span>
                    </div>
                    <div>
                      <b>{fmtTime(hud.workout.stepLeftS)}</b>
                      <span>
                        step {hud.workout.index + 1} of {init.workout.steps.length}
                      </span>
                    </div>
                    <div>
                      {(() => {
                        const next = init.workout.steps[hud.workout.index + 1]
                        if (!next) return <span>last step</span>
                        return (
                          <>
                            <b>{next.kind === 'power' ? `${Math.round(next.from * settings.rider.ftp * hud.intensity)} W` : next.kind === 'max' ? 'All out' : 'Free'}</b>
                            <span>next · {fmtTime(next.durationS)}</span>
                          </>
                        )
                      })()}
                    </div>
                  </div>
                  <WorkoutShape
                    shape={init.workout.steps.flatMap((st, i, all) => {
                      const t0 = all.slice(0, i).reduce((a, x) => a + x.durationS, 0)
                      const lvl = (v: number): number => (st.kind === 'power' ? v : st.kind === 'max' ? 1.6 : 0.6)
                      return [
                        [t0, lvl(st.from)],
                        [t0 + st.durationS, lvl(st.to)]
                      ] as [number, number][]
                    })}
                    height={34}
                    at={hud.workoutTime}
                  />
                  <div className="wk-controls">
                    <button onClick={() => void api.ride.intensity(-0.05)}>−</button>
                    <span>Intensity {Math.round(hud.intensity * 100)}%</span>
                    <button onClick={() => void api.ride.intensity(0.05)}>+</button>
                    <button className="ghost" onClick={() => void api.ride.skipStep()}>
                      Skip step
                    </button>
                  </div>
                </>
              ) : (
                <div className="wk-done">Workout complete: {init.workout.name}. Ride on freely.</div>
              )}
            </div>
          )}
          {hud.cue && <div className="banner cue">{hud.cue}</div>}
          {hud.groupStatus && <div className="group-chip">{hud.groupStatus}</div>}

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
            {settings.look.photos && routeId !== 'demo' && <StreetPhoto distance={hud.distance} />}
          </div>

          {hud.segment && hud.segment.source === 'strava' && (
            <div className="banner climb segment">
              <b>
                ⏱ {hud.segment.name}
                {hud.segment.gap !== null && <em className={hud.segment.gap <= 0 ? 'ahead' : 'behind'}> {fmtGap(hud.segment.gap)} vs your best</em>}
              </b>
              <span>
                {fmtDist(hud.segment.toGo, units, 2)} to go · {fmtTime(hud.segment.elapsed)}
                {hud.segment.best !== null && ` (best ${fmtTime(hud.segment.best)})`}
              </span>
              <div className="bar">
                <div style={{ width: `${Math.min(100, (1 - hud.segment.toGo / hud.segment.length) * 100)}%` }} />
              </div>
            </div>
          )}
          {!(hud.segment && hud.segment.source === 'strava') && hud.segmentResult && hud.segmentResult.source === 'strava' && (
            <div className="banner summit">
              <b>
                {hud.segmentResult.name}: {fmtTime(hud.segmentResult.timeS)}
              </b>
              <span>
                {hud.segmentResult.previousBest === null
                  ? 'First time timed here.'
                  : hud.segmentResult.timeS < hud.segmentResult.previousBest
                    ? `New best, ${fmtTime(hud.segmentResult.previousBest - hud.segmentResult.timeS)} faster!`
                    : `${fmtTime(hud.segmentResult.timeS - hud.segmentResult.previousBest)} off your best.`}
              </span>
            </div>
          )}
          {climb && !(hud.segment && hud.segment.source === 'strava') && (
            <div className="banner climb">
              <b>
                Climb {hud.climbIndex! + 1}
                {climb.category && ` · Cat ${climb.category}`}
              </b>
              <span>
                {fmtDist(Math.max(0, climb.end - hud.distance), units, 2)} to the top · avg {(climb.avgGrade * 100).toFixed(1)}% · {fmtTime(hud.climbElapsed)}
                {climbBest !== undefined && ` (best ${fmtTime(climbBest)})`}
                {hud.segment?.source === 'climb' && hud.segment.gap !== null && ` · ${fmtGap(hud.segment.gap)} vs best`}
              </span>
              <div className="bar">
                <div style={{ width: `${Math.min(100, ((hud.distance - climb.start) / climb.length) * 100)}%` }} />
              </div>
            </div>
          )}
          {!climb && !hud.segment && !hud.segmentResult && hud.climbResult && (
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
              {init.workout && hud.workout ? null : <div className="seg">
                <button className={mode === 'sim' ? 'on' : ''} onClick={() => setTrainer({ mode: 'sim' })} title="Trainer resistance follows the gradient">
                  Gradient
                </button>
                <button className={mode === 'erg' ? 'on' : ''} onClick={() => setTrainer({ mode: 'erg' })} title="Trainer holds a fixed power">
                  ERG
                </button>
              </div>}
              {init.workout && hud.workout ? null : mode === 'erg' ? (
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
              {hud.demoTarget !== null && mode === 'sim' && !(init.workout && hud.workout) && (
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
