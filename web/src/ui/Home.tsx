import { useEffect, useMemo, useState } from 'react'
import { errorText } from '../bridge'
import { fmtDist, fmtEle, fmtTime } from '../core/fmt'
import { api, useSettings, useStored } from '../state'
import type { RideRequest, RouteMeta } from '../types'
import { RideSetup } from './RideSetup'

export function RoutePreview({ outline }: { outline: [number, number][] }): React.JSX.Element {
  const path = useMemo(() => {
    if (outline.length < 2) return ''
    const cos = Math.cos((outline[0][0] * Math.PI) / 180)
    const pts = outline.map(([lat, lon]) => [lon * cos, -lat])
    const xs = pts.map((p) => p[0])
    const ys = pts.map((p) => p[1])
    const minX = Math.min(...xs)
    const minY = Math.min(...ys)
    const scale = Math.min(220 / (Math.max(...xs) - minX || 1), 100 / (Math.max(...ys) - minY || 1))
    const offX = (240 - (Math.max(...xs) - minX) * scale) / 2
    const offY = (120 - (Math.max(...ys) - minY) * scale) / 2
    return pts.map((p, i) => `${i ? 'L' : 'M'}${(offX + (p[0] - minX) * scale).toFixed(1)} ${(offY + (p[1] - minY) * scale).toFixed(1)}`).join('')
  }, [outline])
  return (
    <svg viewBox="0 0 240 120" className="route-preview">
      <path d={path} />
    </svg>
  )
}

export function Home({ onRide }: { onRide: (request: RideRequest) => void }): React.JSX.Element {
  const { settings } = useSettings()
  const [routes, saveRoutes] = useStored<RouteMeta[]>('routes', [])
  const [bests] = useStored<Record<string, number>>('bests', {})
  const [error, setError] = useState('')
  const [dragging, setDragging] = useState(false)
  const [demo, setDemo] = useState<RouteMeta | null>(null)
  const [setup, setSetup] = useState<{ route: RouteMeta; room?: string } | null>(null)
  const [joining, setJoining] = useState(false)
  const [room, setRoom] = useState('')
  useEffect(() => void api.routes.demoMeta().then(setDemo), [])

  const add = (added: RouteMeta[], problems: string[]): void => {
    if (added.length) saveRoutes([...added, ...(routes ?? [])])
    setError(problems.join(' · '))
  }

  const remove = (route: RouteMeta): void => {
    if (!confirm(`Remove “${route.name}” from your library? Your recorded rides are kept.`)) return
    saveRoutes((routes ?? []).filter((r) => r.id !== route.id))
    void api.kv.del(`route-${route.id}`)
    void api.kv.del(`buildings-${route.id}`)
    void api.kv.del(`scenery-${route.id}`)
    void api.kv.del(`ghosts-${route.id}`)
    void api.kv.del(`segments-${route.id}`)
    void api.kv.del(`places-${route.id}`)
  }

  const onDrop = async (e: React.DragEvent): Promise<void> => {
    e.preventDefault()
    setDragging(false)
    const files = [...e.dataTransfer.files].filter((f) => /\.gpx$/i.test(f.name))
    if (!files.length) return setError('Drop .gpx files to import them.')
    const added: RouteMeta[] = []
    const problems: string[] = []
    for (const f of files) {
      const name = f.name.replace(/\.gpx$/i, '')
      try {
        added.push(await api.routes.importText(name, await f.text()))
      } catch (err) {
        problems.push(`${name}: ${errorText(err)}`)
      }
    }
    add(added, problems)
  }

  const card = (r: RouteMeta, builtIn: boolean): React.JSX.Element => (
    <div className="card route" key={r.id}>
      <RoutePreview outline={r.preview} />
      <h3 title={r.name}>{r.name}</h3>
      <div className="route-stats">
        <span>{fmtDist(r.distance, settings.units)}</span>
        <span>↗ {fmtEle(r.ascent, settings.units)}</span>
        <span>
          {r.climbs} climb{r.climbs === 1 ? '' : 's'}
        </span>
      </div>
      <div className="route-best">{bests?.[r.id] ? `Best ${fmtTime(bests[r.id])}` : builtIn ? 'Built-in demo route' : 'Not ridden yet'}</div>
      <div className="row">
        <button className="primary" onClick={() => setSetup({ route: r })}>
          Ride
        </button>
        {!builtIn && (
          <button className="ghost" onClick={() => remove(r)}>
            Remove
          </button>
        )}
      </div>
    </div>
  )

  return (
    <div
      className={dragging ? 'drop-target dragging' : 'drop-target'}
      onDragOver={(e) => {
        e.preventDefault()
        setDragging(true)
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => void onDrop(e)}
    >
      <div className="page-head">
        <div>
          <h1>Your routes</h1>
          <p className="muted">Import a GPX from Strava, Komoot, RideWithGPS or your bike computer — or drag files onto this window.</p>
        </div>
        <div className="row">
          <button onClick={() => setJoining(true)}>Join a group ride</button>
          <button className="primary" onClick={() => void api.routes.importDialog().then((r) => add(r.added, r.problems), (err) => setError(errorText(err)))}>
            Import GPX…
          </button>
        </div>
      </div>
      {joining && (
        <div className="card join">
          <label className="field">
            <span>Room code from the host</span>
            <input value={room} autoFocus onChange={(e) => setRoom(e.target.value.toUpperCase().replace(/[^A-Z0-9-]/g, '').slice(0, 32))} />
          </label>
          <div className="row">
            <button
              className="primary"
              disabled={!room}
              onClick={() =>
                void api.group.fetchRoute(room).then(
                  (meta) => {
                    if (!(routes ?? []).some((r) => r.id === meta.id)) saveRoutes([meta, ...(routes ?? [])])
                    setJoining(false)
                    setSetup({ route: meta, room })
                  },
                  (err) => setError(errorText(err))
                )
              }
            >
              Join
            </button>
            <button onClick={() => setJoining(false)}>Cancel</button>
          </div>
        </div>
      )}
      {setup && <RideSetup route={setup.route} room={setup.room} onCancel={() => setSetup(null)} onStart={(r) => (setSetup(null), onRide(r))} />}
      {error && <div className="error">{error}</div>}
      <div className="grid">
        {(routes ?? []).map((r) => card(r, false))}
        {demo && card(demo, true)}
      </div>
    </div>
  )
}
