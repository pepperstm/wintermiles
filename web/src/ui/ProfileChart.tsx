import { useEffect, useRef } from 'react'
import { gradeColor } from '../core/fmt'
import type { Route } from '../core/gpx'

export interface ChartMarker {
  distance: number
  color: string
}

interface Props {
  route: Route
  distance: number
  /** Distance range to show; omitted = whole route. */
  from?: number
  to?: number
  markers?: ChartMarker[]
}

/** Elevation profile coloured by gradient, with the rider's position. */
export function ProfileChart({ route, distance, from = 0, to = route.total, markers = [] }: Props): React.JSX.Element {
  const ref = useRef<HTMLCanvasElement>(null)

  useEffect(() => {
    const canvas = ref.current
    if (!canvas) return
    const dpr = Math.min(window.devicePixelRatio, 2)
    const w = Math.round(canvas.clientWidth * dpr)
    const h = Math.round(canvas.clientHeight * dpr)
    if (w < 2 || h < 2) return
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w
      canvas.height = h
    }
    const ctx = canvas.getContext('2d')!
    ctx.clearRect(0, 0, w, h)

    const i0 = Math.max(0, Math.floor(from / route.step))
    const i1 = Math.min(route.n - 1, Math.ceil(to / route.step))
    let min = Infinity
    let max = -Infinity
    for (let i = i0; i <= i1; i++) {
      if (route.ele[i] < min) min = route.ele[i]
      if (route.ele[i] > max) max = route.ele[i]
    }
    // Keep some vertical range so a pan-flat stretch doesn't look like a wall.
    const range = Math.max(max - min, Math.min((to - from) * 0.03, 60))
    const pad = 6 * dpr
    const y = (ele: number): number => h - pad - ((ele - min) / range) * (h - pad * 2.5)
    const span = to - from

    for (let px = 0; px < w; px++) {
      const d = from + (px / w) * span
      if (d < 0 || d > route.total) continue
      const f = d / route.step
      const i = Math.min(route.n - 2, Math.floor(f))
      const ele = route.ele[i] + (route.ele[i + 1] - route.ele[i]) * (f - i)
      ctx.fillStyle = gradeColor(route.grade[i])
      ctx.globalAlpha = d <= distance ? 0.35 : 0.95
      const top = y(ele)
      ctx.fillRect(px, top, 1, h - top)
    }
    ctx.globalAlpha = 1

    const mark = (d: number, color: string, radius: number): void => {
      if (d < from || d > to) return
      const px = ((d - from) / span) * w
      const f = Math.min(route.n - 1, d / route.step)
      const ele = route.ele[Math.round(f)]
      ctx.fillStyle = color
      ctx.strokeStyle = '#0e1116'
      ctx.lineWidth = 1.5 * dpr
      ctx.beginPath()
      ctx.arc(px, y(ele), radius * dpr, 0, Math.PI * 2)
      ctx.fill()
      ctx.stroke()
    }
    for (const m of markers) mark(m.distance, m.color, 3.5)
    const px = ((distance - from) / span) * w
    ctx.fillStyle = 'rgba(255,255,255,0.75)'
    ctx.fillRect(px - dpr / 2, 0, dpr, h)
    mark(distance, '#ff5a36', 5)
  })

  return <canvas ref={ref} className="profile-chart" />
}
