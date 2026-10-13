import { useEffect, useRef } from 'react'
import L from 'leaflet'
import 'leaflet/dist/leaflet.css'
import type { Route } from '../core/gpx'

/** A real-world map (OpenStreetMap) following the rider along the route. */
export function MiniMap({ route, lat, lon }: { route: Route; lat: number; lon: number }): React.JSX.Element {
  const el = useRef<HTMLDivElement>(null)
  const map = useRef<L.Map | null>(null)
  const marker = useRef<L.CircleMarker | null>(null)

  useEffect(() => {
    const m = L.map(el.current!, { zoomControl: false, attributionControl: true, dragging: false, scrollWheelZoom: false, doubleClickZoom: false, keyboard: false })
    m.attributionControl.setPrefix(false)
    L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 18, attribution: '© OpenStreetMap' }).addTo(m)
    const line: [number, number][] = []
    const stride = Math.max(1, Math.floor(route.n / 4000))
    for (let i = 0; i < route.n; i += stride) line.push([route.lat[i], route.lon[i]])
    L.polyline(line, { color: '#ff5a36', weight: 4, opacity: 0.9 }).addTo(m)
    marker.current = L.circleMarker([route.lat[0], route.lon[0]], { radius: 7, color: '#fff', weight: 2, fillColor: '#2f7fd8', fillOpacity: 1 }).addTo(m)
    m.setView([route.lat[0], route.lon[0]], 14, { animate: false })
    map.current = m
    return () => {
      m.remove()
      map.current = null
    }
  }, [route])

  useEffect(() => {
    marker.current?.setLatLng([lat, lon])
    map.current?.setView([lat, lon], map.current.getZoom(), { animate: false })
  }, [lat, lon])

  return <div ref={el} className="minimap" />
}
