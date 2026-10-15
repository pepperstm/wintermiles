import { useEffect, useState } from 'react'
import { errorText } from '../bridge'
import { fmtDist, fmtTime } from '../core/fmt'
import { api, useSettings } from '../state'
import type { GhostMeta, RideRequest, RouteMeta, Segment, StravaActivity, WorkoutMeta } from '../types'
import { WorkoutShape } from './Workouts'

const hex = (c: number): string => `#${c.toString(16).padStart(6, '0')}`

/** Chosen before a ride: a workout, ghosts to race, Strava segments and a group-ride room. */
export function RideSetup(props: { route: RouteMeta; room?: string; onCancel: () => void; onStart: (r: RideRequest) => void }): React.JSX.Element {
  const { route } = props
  const { settings } = useSettings()
  const u = settings.units
  const [workouts, setWorkouts] = useState<WorkoutMeta[]>([])
  const [workoutId, setWorkoutId] = useState<string>('')
  const [ghosts, setGhosts] = useState<GhostMeta[]>([])
  const [ghostIds, setGhostIds] = useState<string[]>([])
  const [segments, setSegments] = useState<Segment[]>([])
  const [activities, setActivities] = useState<StravaActivity[] | null>(null)
  const [group, setGroup] = useState(Boolean(props.room))
  const [room, setRoom] = useState(props.room ?? Math.random().toString(36).slice(2, 8).toUpperCase())
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const [stravaConnected, setStravaConnected] = useState(false)
  const isDemo = route.id === 'demo'

  useEffect(() => {
    void api.workouts.list().then(setWorkouts)
    void api.strava.status().then((s) => setStravaConnected(s.connected))
    if (!isDemo) {
      void api.ghosts.list(route.id).then((g) => {
        setGhosts(g)
        setGhostIds(g.map((x) => x.id))
      })
      void api.segments.list(route.id).then(setSegments)
    }
  }, [route.id, isDemo])

  const task = async (label: string, fn: () => Promise<void>): Promise<void> => {
    setBusy(label)
    setError('')
    try {
      await fn()
    } catch (err) {
      setError(errorText(err))
    } finally {
      setBusy('')
    }
  }

  const addGhost = (g: GhostMeta | null): void => {
    if (!g) return
    setGhosts((all) => [...all, g])
    setGhostIds((ids) => [...ids, g.id])
  }

  const chosen = workouts.find((w) => w.id === workoutId)

  return (
    <div className="ride-overlay solid setup-backdrop">
      <div className="modal setup">
        <h2>{route.name}</h2>
        <p className="muted">
          {fmtDist(route.distance, u)} · {route.climbs} climb{route.climbs === 1 ? '' : 's'}
        </p>

        <h3>Workout</h3>
        <select value={workoutId} onChange={(e) => setWorkoutId(e.target.value)}>
          <option value="">No workout: free ride, the gradient sets the effort</option>
          {workouts.map((w) => (
            <option key={w.id} value={w.id}>
              {w.name} · {fmtTime(w.durationS)} · {Math.round(w.stress)} TSS
            </option>
          ))}
        </select>
        {chosen && (
          <>
            <WorkoutShape shape={chosen.shape} height={40} />
            <p className="muted small">
              {chosen.description} {settings.trainer.workoutErg ? 'The trainer holds each target (ERG).' : 'Targets are shown; the trainer follows the gradient.'}
            </p>
          </>
        )}

        {!isDemo && (
          <>
            <h3>Ghosts to race</h3>
            {ghosts.length === 0 && <p className="muted small">Import someone's recorded ride of this road (GPX or FIT, or from Strava) to race it.</p>}
            {ghosts.map((g) => (
              <label key={g.id} className="field toggle ghost-row">
                <input type="checkbox" checked={ghostIds.includes(g.id)} onChange={(e) => setGhostIds((ids) => (e.target.checked ? [...ids, g.id] : ids.filter((i) => i !== g.id)))} />
                <i style={{ background: hex(g.color) }} />
                <span>
                  {g.name} <em className="muted small">{fmtDist(g.distance, u)} in {fmtTime(g.timeS)}</em>
                </span>
                <button className="ghost small" onClick={(e) => {
                    e.preventDefault()
                    void api.ghosts.remove(route.id, g.id).then(() => setGhosts((all) => all.filter((x) => x.id !== g.id)))
                  }}
                >
                  ✕
                </button>
              </label>
            ))}
            <div className="row">
              <button disabled={!!busy} onClick={() => void task('Reading the ride…', async () => addGhost(await api.ghosts.importFile(route.id)))}>
                From a file…
              </button>
              {stravaConnected && (
                <button disabled={!!busy} onClick={() => void task('Loading your Strava rides…', async () => setActivities(await api.stravaActivities()))}>
                  From Strava…
                </button>
              )}
            </div>
            {activities && (
              <div className="pick-list">
                {activities.length === 0 && <div className="muted small">No rides with GPS on your Strava.</div>}
                {activities.map((a) => (
                  <button
                    key={a.id}
                    className="ghost"
                    disabled={!!busy}
                    onClick={() =>
                      void task('Fetching the ride from Strava…', async () => {
                        addGhost(await api.ghosts.importStrava(route.id, a.id))
                        setActivities(null)
                      })
                    }
                  >
                    {a.name} · {a.start.slice(0, 10)} · {fmtDist(a.distance, u)}
                  </button>
                ))}
              </div>
            )}

            <h3>Segments</h3>
            <p className="muted small">
              The route's climbs are always timed. {segments.length ? `${segments.length} Strava segment${segments.length === 1 ? '' : 's'} on this route: ${segments.slice(0, 4).map((s) => s.name).join(', ')}${segments.length > 4 ? '…' : ''}` : 'Strava segments along the route can be timed too.'}
            </p>
            {stravaConnected ? (
              <button disabled={!!busy} onClick={() => void task('Looking for Strava segments…', async () => setSegments(await api.segments.fetch(route.id)))}>
                {segments.length ? 'Refresh Strava segments' : 'Find Strava segments'}
              </button>
            ) : (
              <p className="muted small">Connect Strava in Settings to find segments.</p>
            )}
          </>
        )}

        <h3>Group ride</h3>
        <label className="field toggle">
          <input type="checkbox" checked={group} onChange={(e) => setGroup(e.target.checked)} />
          <span>Ride with friends</span>
          <small>Share the room code; they choose “Join a group ride” on the Routes page. Relay: {settings.group.relayUrl}</small>
        </label>
        {group && (
          <label className="field">
            <span>Room code</span>
            <input value={room} onChange={(e) => setRoom(e.target.value.toUpperCase().replace(/[^A-Z0-9-]/g, '').slice(0, 32))} />
          </label>
        )}

        {busy && <div className="muted">{busy}</div>}
        {error && <div className="error">{error}</div>}
        <div className="row">
          <button className="primary" disabled={!!busy || (group && !room)} onClick={() => props.onStart({ routeId: route.id, workoutId: workoutId || null, ghostIds, groupRoom: group ? room : null })}>
            Start riding
          </button>
          <button onClick={props.onCancel}>Cancel</button>
        </div>
      </div>
    </div>
  )
}
