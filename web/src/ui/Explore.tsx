import { useEffect, useRef, useState } from 'react'
import L from 'leaflet'
import 'leaflet/dist/leaflet.css'
import { errorText } from '../bridge'
import { fmtDist, fmtEle } from '../core/fmt'
import { api, useSettings } from '../state'
import type { RouteMeta } from '../types'

interface Place {
  name: string
  detail: string
  lat: number
  lon: number
}

interface Classic {
  name: string
  region: string
  points: [number, number][]
}

interface Planned {
  gpx: string
  distance: number
  ascent: number
  climbs: number
  line: [number, number][]
  profile: [number, number][]
}

const PROFILES = [
  { id: 'fastbike', label: 'Road bike (fast roads)' },
  { id: 'trekking', label: 'Quiet roads and lanes' },
  { id: 'safety', label: 'Safest (avoid traffic)' }
]

function Elevation({ profile }: { profile: [number, number][] }): React.JSX.Element | null {
  if (profile.length < 2) return null
  const W = 600
  const H = 90
  const total = profile[profile.length - 1][0] || 1
  const lo = Math.min(...profile.map((p) => p[1]))
  const hi = Math.max(lo + 50, ...profile.map((p) => p[1]))
  const pts = profile.map(([d, e]) => `${((d / total) * W).toFixed(1)},${(H - ((e - lo) / (hi - lo)) * (H - 6)).toFixed(1)}`)
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="chart elevation" preserveAspectRatio="none">
      <path d={`M0,${H}L${pts.join('L')}L${W},${H}Z`} />
    </svg>
  )
}

/** Plan a new route on a map, or pick a famous climb. */
export function Explore({ onSaved }: { onSaved: () => void }): React.JSX.Element {
  const { settings } = useSettings()
  const u = settings.units
  const el = useRef<HTMLDivElement>(null)
  const map = useRef<L.Map | null>(null)
  const layer = useRef<L.LayerGroup | null>(null)
  const [points, setPoints] = useState<[number, number][]>([])
  const [profile, setProfile] = useState('fastbike')
  const [planned, setPlanned] = useState<Planned | null>(null)
  const [query, setQuery] = useState('')
  const [places, setPlaces] = useState<Place[]>([])
  const [classics, setClassics] = useState<Classic[]>([])
  const [name, setName] = useState('')
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const [saved, setSaved] = useState('')

  useEffect(() => {
    const m = L.map(el.current!, { zoomControl: true }).setView([46.2, 7.2], 6)
    m.attributionControl.setPrefix(false)
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 18, attribution: '© OpenStreetMap contributors' }).addTo(m)
    layer.current = L.layerGroup().addTo(m)
    m.on('click', (e: L.LeafletMouseEvent) => setPoints((p) => [...p, [e.latlng.lat, e.latlng.lng]]))
    map.current = m
    void api.geo.classics().then(setClassics)
    return () => {
      m.remove()
      map.current = null
    }
  }, [])

  // Draw waypoints and the planned line.
  useEffect(() => {
    const g = layer.current
    if (!g) return
    g.clearLayers()
    if (planned) L.polyline(planned.line, { color: '#ff5a36', weight: 5, opacity: 0.9 }).addTo(g)
    points.forEach((p, i) => {
      const marker = L.marker(p, {
        draggable: true,
        icon: L.divIcon({ className: 'wp', html: `<span>${i === 0 ? 'S' : i === points.length - 1 ? 'F' : i}</span>`, iconSize: [22, 22] })
      })
      marker.on('dragend', () => {
        const ll = marker.getLatLng()
        setPoints((all) => all.map((q, k) => (k === i ? [ll.lat, ll.lng] : q)))
      })
      marker.on('click', () => setPoints((all) => all.filter((_, k) => k !== i)))
      marker.addTo(g)
    })
  }, [points, planned])

  // Route whenever the waypoints or the profile change.
  useEffect(() => {
    setSaved('')
    if (points.length < 2) {
      setPlanned(null)
      return
    }
    let live = true
    const timer = setTimeout(() => {
      setBusy('Planning the route…')
      setError('')
      api.geo.plan(points, profile).then(
        (p) => live && (setPlanned(p), setBusy('')),
        (err) => live && (setError(errorText(err)), setBusy(''))
      )
    }, 400)
    return () => {
      live = false
      clearTimeout(timer)
    }
  }, [points, profile])

  const search = async (): Promise<void> => {
    if (!query.trim()) return
    setBusy('Searching…')
    try {
      setPlaces(await api.geo.search(query))
    } catch (err) {
      setError(errorText(err))
    } finally {
      setBusy('')
    }
  }

  const pickClassic = (c: Classic): void => {
    setName(c.name)
    setPoints(c.points)
    map.current?.fitBounds(L.latLngBounds(c.points).pad(0.4))
  }

  const save = async (): Promise<void> => {
    if (!planned) return
    try {
      const meta = await api.routes.importText(name.trim() || 'Planned route', planned.gpx.replace(/<name>[^<]*<\/name>/, `<name>${escapeXml(name.trim() || 'Planned route')}</name>`))
      const routes = (await api.kv.get<RouteMeta[]>('routes')) ?? []
      await api.kv.set('routes', [meta, ...routes])
      setSaved(meta.name)
      onSaved()
    } catch (err) {
      setError(errorText(err))
    }
  }

  return (
    <div className="explore">
      <div className="explore-side">
        <h1>Find a route</h1>
        <p className="muted small">Click the map to add waypoints from start to finish; drag one to move it, click it to remove it. Routing by BRouter, with real elevation.</p>
        <div className="row">
          <input value={query} placeholder="Search for a place" onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && void search()} />
          <button onClick={() => void search()}>Search</button>
        </div>
        {places.length > 0 && (
          <div className="pick-list">
            {places.map((p, i) => (
              <button
                key={i}
                className="ghost"
                onClick={() => {
                  map.current?.setView([p.lat, p.lon], 13)
                  setPlaces([])
                }}
              >
                <b>{p.name}</b> <span className="muted small">{p.detail}</span>
              </button>
            ))}
          </div>
        )}
        <label className="field">
          <span>Prefer</span>
          <select value={profile} onChange={(e) => setProfile(e.target.value)}>
            {PROFILES.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
        <div className="row">
          <button disabled={!points.length} onClick={() => setPoints((p) => p.slice(0, -1))}>
            Undo
          </button>
          <button disabled={points.length < 2} onClick={() => setPoints((p) => [...p].reverse())}>
            Reverse
          </button>
          <button disabled={!points.length} onClick={() => (setPoints([]), setName(''))}>
            Clear
          </button>
        </div>
        {busy && <div className="muted">{busy}</div>}
        {error && <div className="error">{error}</div>}
        {planned && (
          <div className="card planned">
            <div className="route-stats">
              <span>{fmtDist(planned.distance, u)}</span>
              <span>↗ {fmtEle(planned.ascent, u)}</span>
              <span>
                {planned.climbs} climb{planned.climbs === 1 ? '' : 's'}
              </span>
            </div>
            <Elevation profile={planned.profile} />
            <div className="row">
              <input value={name} placeholder="Name this route" onChange={(e) => setName(e.target.value)} />
              <button className="primary" onClick={() => void save()}>
                Save to library
              </button>
            </div>
            {saved && <div className="ok small">“{saved}” is in your routes, ready to ride.</div>}
          </div>
        )}
        <h2>Famous climbs</h2>
        <div className="pick-list classics">
          {classics.map((c) => (
            <button key={c.name} className="ghost" onClick={() => pickClassic(c)}>
              <b>{c.name}</b> <span className="muted small">{c.region}</span>
            </button>
          ))}
        </div>
      </div>
      <div ref={el} className="explore-map" />
    </div>
  )
}

function escapeXml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}
