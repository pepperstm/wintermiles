import { useEffect, useMemo, useState } from 'react'
import { levelFor } from './core/fmt'
import { api, mergeSettings, refreshDevices, SettingsContext, smokeHooks, useDevices } from './state'
import { EMPTY_PROFILE, type Profile, type RideRequest, type RideSummary, type RouteMeta, type Settings } from './types'
import { Devices } from './ui/Devices'
import { History } from './ui/History'
import { Home } from './ui/Home'
import { Ride } from './ui/Ride'
import { SettingsScreen } from './ui/Settings'
import { Summary } from './ui/Summary'
import { Workouts } from './ui/Workouts'

type Screen =
  | { name: 'home' | 'workouts' | 'devices' | 'history' | 'settings' }
  | { name: 'ride'; request: RideRequest }
  | { name: 'summary'; ride: RideSummary }

const TABS = [
  { name: 'home', label: 'Routes' },
  { name: 'workouts', label: 'Workouts' },
  { name: 'devices', label: 'Devices' },
  { name: 'history', label: 'Progress' },
  { name: 'settings', label: 'Settings' }
] as const

/** Automated check: open a screen, let it run, then report a frame and the visible text. */
async function runSmoke(setScreen: (s: Screen) => void): Promise<void> {
  const smoke = await api.smoke.config()
  if (!smoke) return
  if (smoke.screen === 'ride') {
    // Ride the newest library route (or the demo) with the simulated rider.
    const routes = await api.kv.get<RouteMeta[]>('routes')
    await api.devices.demoAdd()
    setScreen({ name: 'ride', request: { routeId: routes?.[0]?.id ?? 'demo', workoutId: smoke.workout, groupRoom: smoke.room, ghostIds: smoke.ghosts ?? [] } })
  } else if (smoke.screen === 'devices' || smoke.screen === 'history' || smoke.screen === 'settings' || smoke.screen === 'workouts' || smoke.screen === 'home') {
    if (smoke.screen === 'devices') await api.devices.demoAdd()
    setScreen({ name: smoke.screen })
  }
  setTimeout(() => {
    const tiles = document.querySelectorAll('.leaflet-tile-loaded').length
    const info = `${document.body.innerText}\n\n--- map tiles loaded: ${tiles}\n--- world: ${JSON.stringify(smokeHooks.world)}\n--- frames drawn: ${(smokeHooks.world as { frames?: () => number } | null)?.frames?.() ?? 0}\n--- last ride state: ${JSON.stringify(smokeHooks.state)}`
    void api.smoke.done(smokeHooks.capture?.() ?? null, info)
  }, smoke.ms)
}

export function App(): React.JSX.Element | null {
  const [settings, setSettings] = useState<Settings | null>(null)
  const [profile, setProfile] = useState<Profile>(EMPTY_PROFILE)
  const [screen, setScreen] = useState<Screen>({ name: 'home' })
  const { devices } = useDevices()

  useEffect(() => {
    void api.kv.get<Partial<Settings>>('settings').then((s) => {
      setSettings(mergeSettings(s))
      void runSmoke(setScreen)
    })
  }, [])

  // The profile changes when a ride is saved, so refresh it whenever a screen opens.
  useEffect(() => {
    void api.kv.get<Profile>('profile').then((p) => setProfile(p ?? EMPTY_PROFILE))
    void refreshDevices()
  }, [screen])

  const store = useMemo(
    () => ({
      settings: settings!,
      update: (patch: (s: Settings) => Settings) =>
        setSettings((prev) => {
          const next = patch(prev!)
          void api.kv.set('settings', next)
          // A ride in progress picks up trainer mode, ERG target and difficulty changes live.
          void api.ride.updateSettings(next)
          return next
        })
    }),
    [settings]
  )
  if (!settings) return null

  let body: React.JSX.Element
  if (screen.name === 'ride') {
    body = (
      <Ride
        request={screen.request}
        onExit={() => setScreen({ name: 'home' })}
        onDevices={() => setScreen({ name: 'devices' })}
        onFinish={(ride) => setScreen({ name: 'summary', ride })}
      />
    )
  } else {
    const level = levelFor(profile.xp)
    const connected = devices.filter((d) => d.status === 'connected').length
    body = (
      <div className="shell">
        <header className="topbar">
          <div className="brand">
            <span className="brand-mark">❄</span> WinterMiles
          </div>
          <nav>
            {TABS.map((t) => (
              <button key={t.name} className={screen.name === t.name ? 'tab active' : 'tab'} onClick={() => setScreen({ name: t.name })}>
                {t.label}
              </button>
            ))}
          </nav>
          <div className="topbar-right">
            <button className={connected ? 'chip ok' : 'chip'} onClick={() => setScreen({ name: 'devices' })}>
              {connected ? `${connected} sensor${connected > 1 ? 's' : ''} connected` : 'No sensors'}
            </button>
            <div className="level" title={`${profile.xp} XP`}>
              <span>Lv {level.level}</span>
              <div className="bar">
                <div style={{ width: `${(level.into / level.span) * 100}%` }} />
              </div>
            </div>
          </div>
        </header>
        <main className="page">
          {screen.name === 'home' && <Home onRide={(request) => setScreen({ name: 'ride', request })} />}
          {screen.name === 'workouts' && <Workouts />}
          {screen.name === 'devices' && <Devices />}
          {screen.name === 'history' && <History profile={profile} />}
          {screen.name === 'settings' && <SettingsScreen />}
          {screen.name === 'summary' && <Summary ride={screen.ride} onDone={() => setScreen({ name: 'home' })} />}
        </main>
      </div>
    )
  }
  return <SettingsContext.Provider value={store}>{body}</SettingsContext.Provider>
}
