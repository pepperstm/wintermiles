import { useEffect, useState } from 'react'
import { distUnit, distValue, fmtDist } from '../core/fmt'
import { api, useSettings } from '../state'
import type { FitnessOverview } from '../types'

const W = 600
const H = 160

function label(seconds: number): string {
  return seconds < 60 ? `${seconds}s` : seconds < 3600 ? `${seconds / 60}m` : `${seconds / 3600}h`
}

/** Best power for each effort length: all time, and the last six weeks. */
function PowerCurve({ data }: { data: FitnessOverview }): React.JSX.Element {
  const top = Math.max(100, ...data.curveAll) * 1.1
  const x = (i: number): number => 30 + (i / (data.durations.length - 1)) * (W - 40)
  const y = (w: number): number => H - 20 - (w / top) * (H - 30)
  const line = (curve: number[]): string =>
    curve
      .map((w, i) => [i, w] as const)
      .filter(([, w]) => w > 0)
      .map(([i, w], k) => `${k ? 'L' : 'M'}${x(i).toFixed(1)} ${y(w).toFixed(1)}`)
      .join('')
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="chart">
      {data.durations.map((d, i) => (
        <text key={d} x={x(i)} y={H - 4} textAnchor="middle">
          {label(d)}
        </text>
      ))}
      <path d={line(data.curveAll)} className="series dim" />
      <path d={line(data.curveRecent)} className="series" />
      {data.curveRecent.map((w, i) =>
        w > 0 ? (
          <text key={i} x={x(i)} y={y(w) - 6} textAnchor="middle" className="value">
            {Math.round(w)}
          </text>
        ) : null
      )}
    </svg>
  )
}

/** Fitness, fatigue and form over the last months. */
function LoadChart({ data }: { data: FitnessOverview }): React.JSX.Element {
  const days = data.load
  if (days.length < 2) return <div className="empty">Ride a few times to see your training load.</div>
  const top = Math.max(20, ...days.map((d) => Math.max(d.fitness, d.fatigue))) * 1.1
  const low = Math.min(-10, ...days.map((d) => d.form)) * 1.1
  const x = (i: number): number => 10 + (i / (days.length - 1)) * (W - 20)
  const y = (v: number): number => 10 + ((top - v) / (top - low)) * (H - 30)
  const line = (key: 'fitness' | 'fatigue' | 'form'): string => days.map((d, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)} ${y(d[key]).toFixed(1)}`).join('')
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="chart">
      <line x1={10} x2={W - 10} y1={y(0)} y2={y(0)} className="axis" />
      {days.map((d, i) => (d.tss > 0 ? <rect key={i} x={x(i) - 1.5} width={3} y={y(Math.min(d.tss, top))} height={y(0) - y(Math.min(d.tss, top))} className="bar-tss" /> : null))}
      <path d={line('fitness')} className="series fitness" />
      <path d={line('fatigue')} className="series fatigue" />
      <path d={line('form')} className="series form" />
    </svg>
  )
}

export function Fitness(): React.JSX.Element | null {
  const { settings, update } = useSettings()
  const [data, setData] = useState<FitnessOverview | null>(null)
  useEffect(() => void api.fitness().then(setData), [])
  if (!data) return null
  const u = settings.units
  const g = data.goal
  const pct = g.targetKm > 0 ? Math.min(100, (g.doneKm / g.targetKm) * 100) : 0
  const today = data.load[data.load.length - 1]
  const est = data.ftpEstimate
  const topWeek = Math.max(1, ...data.weeks.map((w) => w.km))
  return (
    <>
      <h2>Season goal</h2>
      <div className="card goal">
        <div>
          <b>{g.name}</b>
          <span className="muted small">
            {' '}
            {g.from} → {g.to}
          </span>
        </div>
        <div className="bar big">
          <div style={{ width: `${pct}%` }} />
        </div>
        <div className="muted small">
          {fmtDist(g.doneKm * 1000, u, 0)} of {fmtDist(g.targetKm * 1000, u, 0)} ·{' '}
          {g.doneKm >= g.targetKm
            ? 'Goal reached!'
            : `${g.daysLeft} days left · ${distValue(g.perWeekKm * 1000, u).toFixed(0)} ${distUnit(u)} a week to make it`}
        </div>
        <div className="streaks">
          <span>
            <b>{data.streaks.weeks}</b> week streak
          </span>
          <span>
            <b>{data.streaks.days}</b> day streak
          </span>
          <span>
            <b>{data.streaks.bestWeeks}</b> best weeks in a row
          </span>
        </div>
        <div className="weeks">
          {data.weeks.map((w) => (
            <div key={w.day} title={`${new Date(w.day * 86400000).toLocaleDateString()}: ${fmtDist(w.km * 1000, u)} · ${w.hours.toFixed(1)} h · ${Math.round(w.tss)} TSS`}>
              <i style={{ height: `${(w.km / topWeek) * 100}%` }} />
            </div>
          ))}
        </div>
        <div className="muted small">Distance per week, last 12 weeks. Change the goal in Settings.</div>
      </div>

      <h2>Fitness</h2>
      <div className="fitness-grid">
        <div className="card">
          <h3>Training load</h3>
          <LoadChart data={data} />
          <div className="legend">
            <span className="fitness">Fitness {today ? Math.round(today.fitness) : 0}</span>
            <span className="fatigue">Fatigue {today ? Math.round(today.fatigue) : 0}</span>
            <span className="form">Form {today ? Math.round(today.form) : 0}</span>
          </div>
          <p className="muted small">Fitness is your 6-week training load, fatigue the last week's; form is the difference. Bars are each day's TSS.</p>
        </div>
        <div className="card">
          <h3>Power curve</h3>
          <PowerCurve data={data} />
          <div className="legend">
            <span className="form">Last 6 weeks</span>
            <span className="dim">All time</span>
          </div>
          {est ? (
            <p>
              Estimated FTP: <b>{est.watts} W</b> <span className="muted small">({est.method}, last 6 weeks)</span>{' '}
              {Math.abs(est.watts - settings.rider.ftp) >= 3 && (
                <button className="small" onClick={() => update((s) => ({ ...s, rider: { ...s.rider, ftp: est.watts } }))}>
                  Use {est.watts} W (now {settings.rider.ftp} W)
                </button>
              )}
            </p>
          ) : (
            <p className="muted small">Ride 20 minutes hard, or take the FTP ramp test from Workouts, to get an FTP estimate.</p>
          )}
        </div>
      </div>
    </>
  )
}
