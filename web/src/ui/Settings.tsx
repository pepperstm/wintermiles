import { useEffect, useState } from 'react'
import { errorText } from '../bridge'
import { api, useSettings } from '../state'
import type { HealthSummary, Settings, StravaStatus } from '../types'

function Num(props: {
  label: string
  value: number
  onChange: (v: number) => void
  min: number
  max: number
  step?: number
  suffix?: string
  hint?: string
}): React.JSX.Element {
  // Keep the raw text so half-typed values ("7", "") aren't clamped mid-edit.
  const [text, setText] = useState(String(props.value))
  useEffect(() => setText(String(props.value)), [props.value])
  const commit = (): void => {
    const v = parseFloat(text)
    if (isFinite(v)) props.onChange(Math.max(props.min, Math.min(props.max, v)))
    else setText(String(props.value))
  }
  return (
    <label className="field">
      <span>{props.label}</span>
      <div className="input-row">
        <input
          type="number"
          value={text}
          min={props.min}
          max={props.max}
          step={props.step ?? 1}
          onChange={(e) => setText(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => e.key === 'Enter' && commit()}
        />
        {props.suffix && <em>{props.suffix}</em>}
      </div>
      {props.hint && <small>{props.hint}</small>}
    </label>
  )
}

function Toggle(props: { label: string; value: boolean; onChange: (v: boolean) => void; hint?: string }): React.JSX.Element {
  return (
    <label className="field toggle">
      <input type="checkbox" checked={props.value} onChange={(e) => props.onChange(e.target.checked)} />
      <span>{props.label}</span>
      {props.hint && <small>{props.hint}</small>}
    </label>
  )
}

function StravaCard(): React.JSX.Element {
  const { settings, update } = useSettings()
  const [status, setStatus] = useState<StravaStatus | null>(null)
  const [clientId, setClientId] = useState('')
  const [secret, setSecret] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    void api.strava.status().then((s) => {
      setStatus(s)
      setClientId(s.clientId)
    })
  }, [])

  const run = async (fn: () => Promise<StravaStatus>): Promise<void> => {
    setBusy(true)
    setError('')
    try {
      setStatus(await fn())
    } catch (err) {
      setError(errorText(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="card">
      <h2>Strava</h2>
      {status?.connected ? (
        <>
          <p>
            Connected{status.athlete ? ` as ${status.athlete}` : ''}. Rides upload as <b>Virtual Rides</b> with power, heart rate and the route map.
          </p>
          <Toggle
            label="Upload every ride automatically"
            value={settings.stravaAutoUpload}
            onChange={(v) => update((s) => ({ ...s, stravaAutoUpload: v }))}
          />
          <button disabled={busy} onClick={() => void run(api.strava.disconnect)}>
            Disconnect
          </button>
        </>
      ) : (
        <>
          <p className="muted">
            Strava gives every app its own key, and an open-source app can't ship one — so you create a free personal key once (about a minute):
          </p>
          <ol className="muted steps">
            <li>
              Open{' '}
              <a href="#" onClick={() => void api.openExternal('https://www.strava.com/settings/api')}>
                strava.com/settings/api
              </a>{' '}
              and create an application (any name and website).
            </li>
            <li>
              Set <b>Authorization Callback Domain</b> to <code>localhost</code>.
            </li>
            <li>Paste the Client ID and Client Secret below, then connect.</li>
          </ol>
          <div className="fields">
            <label className="field">
              <span>Client ID</span>
              <input value={clientId} onChange={(e) => setClientId(e.target.value)} inputMode="numeric" />
            </label>
            <label className="field">
              <span>Client secret</span>
              <input
                type="password"
                value={secret}
                placeholder={status?.configured ? 'saved — leave blank to keep' : ''}
                onChange={(e) => setSecret(e.target.value)}
              />
            </label>
          </div>
          <button
            className="primary"
            disabled={busy || !clientId.trim() || (!secret.trim() && !status?.configured)}
            onClick={() =>
              void run(async () => {
                await api.strava.setApp(clientId, secret)
                setSecret('')
                return api.strava.connect()
              })
            }
          >
            {busy ? 'Waiting for Strava in your browser…' : 'Connect to Strava'}
          </button>
        </>
      )}
      {error && <div className="error">{error}</div>}
    </section>
  )
}

function HealthCard(): React.JSX.Element {
  const { settings, update } = useSettings()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [health, setHealth] = useState<HealthSummary | null>(null)

  const run = async (): Promise<void> => {
    setBusy(true)
    setError('')
    try {
      setHealth(await api.health.import())
    } catch (err) {
      setError(errorText(err))
    } finally {
      setBusy(false)
    }
  }

  const apply = (patch: Partial<Settings['rider']>): void => update((s) => ({ ...s, rider: { ...s.rider, ...patch } }))

  return (
    <section className="card">
      <h2>Apple Health &amp; Apple Watch</h2>
      <p className="muted">
        Apple only lets apps on your iPhone talk to Health directly, so a desktop app works with it in three ways:
      </p>
      <ul className="muted steps">
        <li>
          <b>Bring your body data in:</b> on your iPhone open Health → profile picture → <i>Export All Health Data</i>, send the zip to this
          computer and import it here to set weight, FTP and max heart rate.
        </li>
        <li>
          <b>Live Apple Watch heart rate:</b> run a broadcaster app on the watch and add it under Devices as a Bluetooth sensor.
        </li>
        <li>
          <b>Send rides to Health:</b> every ride is saved as a .FIT file. Point the export folder below at iCloud Drive, then open the file on
          your iPhone with a FIT importer such as HealthFit or RunGap, which writes the workout into Health.
        </li>
      </ul>
      <div className="row wrap">
        <button className="primary" disabled={busy} onClick={() => void run()}>
          {busy ? 'Reading export (can take a minute)…' : 'Import Apple Health export…'}
        </button>
      </div>
      {error && <div className="error">{error}</div>}
      {health && (
        <div className="health">
          <div className="muted small">
            Read {health.records.toLocaleString()} records · {health.cyclingWorkouts} cycling workouts
            {health.vo2max ? ` · VO₂max ${health.vo2max}` : ''}
            {health.restingHr ? ` · resting HR ${health.restingHr}` : ''}
          </div>
          {health.weightKg && (
            <div className="row">
              <span>
                Weight <b>{health.weightKg} kg</b> ({health.weightDate})
              </span>
              <button disabled={settings.rider.weightKg === health.weightKg} onClick={() => apply({ weightKg: health.weightKg! })}>
                Use
              </button>
            </div>
          )}
          {health.ftp && (
            <div className="row">
              <span>
                Cycling FTP <b>{health.ftp} W</b>
              </span>
              <button disabled={settings.rider.ftp === health.ftp} onClick={() => apply({ ftp: health.ftp! })}>
                Use
              </button>
            </div>
          )}
          {health.maxHrObserved && (
            <div className="row">
              <span>
                Highest heart rate in the last year <b>{health.maxHrObserved} bpm</b>
              </span>
              <button disabled={settings.rider.maxHr === health.maxHrObserved} onClick={() => apply({ maxHr: health.maxHrObserved! })}>
                Use as max HR
              </button>
            </div>
          )}
          {!health.weightKg && !health.ftp && !health.maxHrObserved && <div className="muted">No weight, FTP or heart-rate data in this export.</div>}
        </div>
      )}
    </section>
  )
}

function FanCard(): React.JSX.Element {
  const { settings, update } = useSettings()
  const smart = settings.smart
  const set = (patch: Partial<Settings['smart']>): void => update((s) => ({ ...s, smart: { ...s.smart, ...patch } }))
  const [token, setToken] = useState('')
  const [status, setStatus] = useState('')
  const unit = smart.fanBy === 'power' ? 'W' : smart.fanBy === 'hr' ? 'bpm' : 'km/h'
  return (
    <section className="card">
      <h2>Fan &amp; lights</h2>
      <p className="muted">Switch a fan on (and faster) as you speed up or work harder, and colour a light by power zone.</p>
      <div className="fields">
        <label className="field">
          <span>Connection</span>
          <select value={smart.kind} onChange={(e) => set({ kind: e.target.value as Settings['smart']['kind'] })}>
            <option value="none">None</option>
            <option value="homeassistant">Home Assistant</option>
            <option value="shelly">Shelly plug</option>
            <option value="tasmota">Tasmota plug</option>
            <option value="webhook">Web address (webhook)</option>
          </select>
        </label>
        {smart.kind !== 'none' && (
          <label className="field">
            <span>{smart.kind === 'homeassistant' ? 'Home Assistant address' : smart.kind === 'webhook' ? 'Address to call' : "Plug's IP address"}</span>
            <input
              value={smart.url}
              onChange={(e) => set({ url: e.target.value.trim() })}
              placeholder={smart.kind === 'homeassistant' ? 'http://homeassistant.local:8123' : smart.kind === 'webhook' ? 'http://…/fan?level={level}&zone={zone}' : '192.168.1.40'}
            />
            {smart.kind === 'webhook' && <small>{'{level}'} (0–100), {'{zone}'} (1–7), {'{power}'}, {'{speed}'} and {'{hr}'} are filled in.</small>}
          </label>
        )}
        {smart.kind === 'homeassistant' && (
          <>
            <label className="field">
              <span>Long-lived access token</span>
              <input type="password" value={token} placeholder="kept in the system keychain" onChange={(e) => setToken(e.target.value)} onBlur={() => token && void api.smart.setToken(token).then(() => (setToken(''), setStatus('Token saved.')))} />
            </label>
            <label className="field">
              <span>Fan entity</span>
              <input value={smart.fanEntity} onChange={(e) => set({ fanEntity: e.target.value.trim() })} placeholder="fan.desk_fan or switch.fan_plug" />
            </label>
            <label className="field">
              <span>Light entity</span>
              <input value={smart.lightEntity} onChange={(e) => set({ lightEntity: e.target.value.trim() })} placeholder="light.pain_cave" />
            </label>
          </>
        )}
        {smart.kind !== 'none' && (
          <>
            <label className="field">
              <span>Fan follows</span>
              <select value={smart.fanBy} onChange={(e) => set({ fanBy: e.target.value as Settings['smart']['fanBy'] })}>
                <option value="speed">Speed</option>
                <option value="power">Power</option>
                <option value="hr">Heart rate</option>
              </select>
            </label>
            <Num label="Fan on from" value={smart.fanFrom} min={0} max={1000} suffix={unit} onChange={(v) => set({ fanFrom: v })} />
            <Num label="Full speed at" value={smart.fanFull} min={0} max={1000} suffix={unit} onChange={(v) => set({ fanFull: v })} />
          </>
        )}
      </div>
      {smart.kind !== 'none' && (
        <button
          onClick={() => {
            setStatus('Fan on full for five seconds…')
            void api.smart.test().then(
              () => setStatus('Test sent. Did the fan come on and go off again?'),
              (err) => setStatus(errorText(err))
            )
          }}
        >
          Test
        </button>
      )}
      {status && <div className="muted small">{status}</div>}
    </section>
  )
}

export function SettingsScreen(): React.JSX.Element {
  const { settings, update } = useSettings()
  const rider = (patch: Partial<Settings['rider']>): void => update((s) => ({ ...s, rider: { ...s.rider, ...patch } }))
  const physics = (patch: Partial<Settings['physics']>): void => update((s) => ({ ...s, physics: { ...s.physics, ...patch } }))
  const trainer = (patch: Partial<Settings['trainer']>): void => update((s) => ({ ...s, trainer: { ...s.trainer, ...patch } }))
  const look = (patch: Partial<Settings['look']>): void => update((s) => ({ ...s, look: { ...s.look, ...patch } }))
  const goal = (patch: Partial<Settings['goal']>): void => update((s) => ({ ...s, goal: { ...s.goal, ...patch } }))

  return (
    <div className="settings">
      <div className="page-head">
        <h1>Settings</h1>
      </div>

      <section className="card">
        <h2>Rider</h2>
        <div className="fields">
          <label className="field">
            <span>Name</span>
            <input value={settings.rider.name} onChange={(e) => rider({ name: e.target.value })} />
          </label>
          <Num label="Weight" value={settings.rider.weightKg} min={30} max={200} step={0.1} suffix="kg" onChange={(v) => rider({ weightKg: v })} />
          <Num label="Bike weight" value={settings.rider.bikeKg} min={4} max={30} step={0.1} suffix="kg" onChange={(v) => rider({ bikeKg: v })} />
          <Num label="FTP" value={settings.rider.ftp} min={50} max={600} suffix="W" hint="Sets power-zone colours and TSS." onChange={(v) => rider({ ftp: v })} />
          <Num label="Max heart rate" value={settings.rider.maxHr} min={100} max={230} suffix="bpm" onChange={(v) => rider({ maxHr: v })} />
          <label className="field">
            <span>Units</span>
            <select value={settings.units} onChange={(e) => update((s) => ({ ...s, units: e.target.value as Settings['units'] }))}>
              <option value="metric">Metric (km, m)</option>
              <option value="imperial">Imperial (mi, ft)</option>
            </select>
          </label>
        </div>
      </section>

      <section className="card">
        <h2>Trainer &amp; physics</h2>
        <div className="fields">
          <Num
            label="Trainer difficulty"
            value={Math.round(settings.trainer.difficulty * 100)}
            min={0}
            max={100}
            step={5}
            suffix="%"
            hint="How much of the real gradient your trainer applies. Your virtual speed always uses the full gradient."
            onChange={(v) => trainer({ difficulty: v / 100 })}
          />
          <Num
            label="Steepest gradient"
            value={Math.round(settings.trainer.maxGrade * 100)}
            min={0}
            max={30}
            suffix="%"
            hint="The trainer never goes steeper than this (after difficulty). 0 means no limit."
            onChange={(v) => trainer({ maxGrade: v / 100 })}
          />
          <Num
            label="Downhill feel"
            value={Math.round(settings.trainer.downhill * 100)}
            min={0}
            max={100}
            step={5}
            suffix="%"
            hint="Share of descents passed on, so you don't spin out on the way down."
            onChange={(v) => trainer({ downhill: v / 100 })}
          />
          <Num label="Drag area (CdA)" value={settings.physics.cda} min={0.15} max={0.7} step={0.01} suffix="m²" hint="≈0.32 on the hoods, 0.25 in an aero tuck." onChange={(v) => physics({ cda: v })} />
          <Num label="Rolling resistance (Crr)" value={settings.physics.crr} min={0.002} max={0.02} step={0.0005} hint="0.004 road tyres on tarmac, 0.008 gravel." onChange={(v) => physics({ crr: v })} />
          <Num label="Descent speed limit" value={settings.physics.maxSpeedKph} min={30} max={120} suffix="km/h" hint="Stands in for braking into corners." onChange={(v) => physics({ maxSpeedKph: v })} />
        </div>
        <Toggle
          label="Estimate power from a speed sensor"
          hint="For basic (non-smart) trainers with no power meter. Uses a typical fluid-trainer curve, so treat the watts as approximate."
          value={settings.trainer.virtualPower}
          onChange={(v) => trainer({ virtualPower: v })}
        />
        <Toggle
          label="Hold workout targets (ERG)"
          hint="During a structured workout the trainer sets the resistance for each target. Off: targets are shown, and the gradient sets the resistance."
          value={settings.trainer.workoutErg}
          onChange={(v) => trainer({ workoutErg: v })}
        />
      </section>

      <section className="card">
        <h2>Virtual world</h2>
        <Toggle
          label="Real terrain around the route"
          hint="Downloads open elevation data for the area so the mountains match the real place. Off (or offline) uses generated hills."
          value={settings.realTerrain}
          onChange={(v) => update((s) => ({ ...s, realTerrain: v }))}
        />
        <Toggle
          label="Buildings from OpenStreetMap"
          hint="Downloads the building outlines along each route once (then kept on this computer) and stands them in the landscape."
          value={settings.buildings}
          onChange={(v) => update((s) => ({ ...s, buildings: v }))}
        />
        <Toggle
          label="Forests, water, roads, bridges and tunnels from OpenStreetMap"
          hint="Downloads the map of each route's surroundings once: woods are planted where they really are, lakes and rivers are filled, the other roads, paths and railways are laid out, and your road is carried over bridges and through tunnels where the real one is."
          value={settings.scenery}
          onChange={(v) => update((s) => ({ ...s, scenery: v }))}
        />
        <div className="fields">
          <label className="field">
            <span>Time of day</span>
            <select value={settings.look.timeOfDay} onChange={(e) => look({ timeOfDay: e.target.value as Settings['look']['timeOfDay'] })}>
              <option value="dawn">Dawn</option>
              <option value="day">Day</option>
              <option value="evening">Evening</option>
              <option value="dusk">Dusk</option>
              <option value="night">Night</option>
              <option value="clock">Same as now</option>
            </select>
          </label>
          <label className="field">
            <span>Weather</span>
            <select value={settings.look.weather} onChange={(e) => look({ weather: e.target.value as Settings['look']['weather'] })}>
              <option value="clear">Clear</option>
              <option value="cloudy">Cloudy</option>
              <option value="rain">Rain</option>
              <option value="fog">Fog</option>
              <option value="snow">Snow</option>
              <option value="live">The weather there right now</option>
            </select>
          </label>
        </div>
        <Toggle
          label="Place names, summits and points of interest"
          hint="Village signs, summit boards with the climb's height, cafés, viewpoints and more along the route, from OpenStreetMap."
          value={settings.look.landmarks}
          onChange={(v) => look({ landmarks: v })}
        />
        <Toggle
          label="Street-level photos from Mapillary"
          hint="Shows real photos taken along the road, where Mapillary has them. Needs a free client token from mapillary.com/dashboard/developers."
          value={settings.look.photos}
          onChange={(v) => look({ photos: v })}
        />
        {settings.look.photos && (
          <label className="field">
            <span>Mapillary client token</span>
            <input type="password" value={settings.look.mapillaryToken} placeholder="MLY|…" onChange={(e) => look({ mapillaryToken: e.target.value.trim() })} />
          </label>
        )}
        <Toggle
          label="Native renderer (experimental, macOS)"
          hint="Draws the 3D world straight on the graphics card (Metal) instead of inside the app's web page, for lower overhead. Not yet drawn natively: rain and snow particles, and the riders' shadows."
          value={settings.nativeRenderer}
          onChange={(v) => update((s) => ({ ...s, nativeRenderer: v }))}
        />
        <Toggle
          label="Pace riders"
          hint="Computer riders holding a steady power to chase or drop."
          value={settings.bots.enabled}
          onChange={(v) => update((s) => ({ ...s, bots: { ...s.bots, enabled: v } }))}
        />
        {settings.bots.enabled && (
          <div className="fields">
            {settings.bots.wkg.map((w, i) => (
              <Num
                key={i}
                label={`Pace rider ${i + 1}`}
                value={w}
                min={0.5}
                max={6}
                step={0.1}
                suffix="W/kg"
                onChange={(v) => update((s) => ({ ...s, bots: { ...s.bots, wkg: s.bots.wkg.map((x, k) => (k === i ? v : x)) } }))}
              />
            ))}
          </div>
        )}
      </section>

      <section className="card">
        <h2>Season goal</h2>
        <div className="fields">
          <label className="field">
            <span>Name</span>
            <input value={settings.goal.name} onChange={(e) => goal({ name: e.target.value })} />
          </label>
          <Num label="Distance" value={settings.goal.distanceKm} min={0} max={50000} step={50} suffix="km" onChange={(v) => goal({ distanceKm: v })} />
          <label className="field">
            <span>Counting from</span>
            <input type="date" value={settings.goal.from} onChange={(e) => goal({ from: e.target.value })} />
            <small>Empty: 1 September.</small>
          </label>
          <label className="field">
            <span>Deadline</span>
            <input type="date" value={settings.goal.deadline} onChange={(e) => goal({ deadline: e.target.value })} />
            <small>Empty: the first day of spring (20 March).</small>
          </label>
        </div>
      </section>

      <FanCard />

      <section className="card">
        <h2>Group rides</h2>
        <p className="muted">
          Riding together goes through a small relay server that passes everyone's position around. Anyone can run one: it is the{' '}
          <code>wintermiles-relay</code> program in this app's source (<code>cargo run --release -p wintermiles-relay</code>), and needs port 47800 open. Everyone in the group
          enters the same address here.
        </p>
        <label className="field">
          <span>Relay address</span>
          <input value={settings.group.relayUrl} onChange={(e) => update((s) => ({ ...s, group: { relayUrl: e.target.value.trim() } }))} placeholder="ws://relay.example.org:47800" />
        </label>
      </section>

      <StravaCard />
      <HealthCard />

      <section className="card">
        <h2>Ride files</h2>
        <p className="muted">
          Every ride is saved as a .FIT file. Optionally copy each one to a second folder — for example iCloud Drive or Dropbox, so it is on your
          phone straight away.
        </p>
        <div className="row wrap">
          <code>{settings.exportDir || 'No extra export folder'}</code>
          <button onClick={() => void api.chooseDir().then((dir) => dir && update((s) => ({ ...s, exportDir: dir })))}>Choose folder…</button>
          {settings.exportDir && <button onClick={() => update((s) => ({ ...s, exportDir: '' }))}>Clear</button>}
        </div>
      </section>
    </div>
  )
}
