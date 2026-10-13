import { fmtDist, fmtEle, fmtTime, levelFor } from '../core/fmt'
import { api, useBadges, useSettings, useStored } from '../state'
import type { Profile, RideSummary } from '../types'
import { StravaUpload } from './Summary'

export function History({ profile }: { profile: Profile }): React.JSX.Element {
  const { settings } = useSettings()
  const [rides] = useStored<RideSummary[]>('rides', [])
  const level = levelFor(profile.xp)
  const badges = useBadges()
  const u = settings.units

  return (
    <div>
      <div className="page-head">
        <h1>Progress</h1>
      </div>
      <div className="card progress">
        <div className="level-big">
          <div className="level-num">Level {level.level}</div>
          <div className="bar">
            <div style={{ width: `${(level.into / level.span) * 100}%` }} />
          </div>
          <div className="muted small">
            {level.into} / {level.span} XP to level {level.level + 1}
          </div>
        </div>
        <div className="stat">
          <div className="stat-value">{profile.rides}</div>
          <div className="stat-label">Rides</div>
        </div>
        <div className="stat">
          <div className="stat-value">{fmtDist(profile.totalDistance, u, 0)}</div>
          <div className="stat-label">Distance</div>
        </div>
        <div className="stat">
          <div className="stat-value">{fmtEle(profile.totalAscent, u)}</div>
          <div className="stat-label">Climbed</div>
        </div>
      </div>

      <h2>Badges</h2>
      <div className="badges">
        {badges.map((b) => (
          <div className={profile.badges.includes(b.id) ? 'badge earned' : 'badge'} key={b.id}>
            <b>{b.name}</b>
            <span>{b.description}</span>
          </div>
        ))}
      </div>

      <h2>Rides</h2>
      <div className="list">
        {rides?.length === 0 && <div className="empty">No rides recorded yet.</div>}
        {rides?.map((r) => (
          <div className="card ride-row" key={r.id}>
            <div>
              <h3>
                {r.routeName} {r.isPb && <span className="tag">PB</span>} {!r.completed && <span className="tag dim">partial</span>}
              </h3>
              <div className="muted small">
                {new Date(r.startedAt).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })}
              </div>
            </div>
            <div className="readings">
              <span>{fmtDist(r.distanceM, u)}</span>
              <span>{fmtTime(r.durationS)}</span>
              <span>{Math.round(r.avgPower)} W</span>
              <span>↗ {fmtEle(r.ascentM, u)}</span>
            </div>
            <div className="row">
              <StravaUpload ride={r} />
              <button className="ghost" onClick={() => void api.revealFile(r.filePath)}>
                File
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
