import { useEffect, useState } from 'react'
import { errorText, platform } from '../bridge'
import { api, refreshDevices, useDevices } from '../state'
import type { BtCandidate, DeviceInfo } from '../types'

const KIND_LABEL: Record<DeviceInfo['kind'], string> = {
  trainer: 'Smart trainer',
  power: 'Power meter',
  hr: 'Heart rate',
  cadence: 'Cadence',
  speed: 'Speed'
}

const STATUS_LABEL: Record<DeviceInfo['status'], string> = {
  connecting: 'Connecting…',
  connected: 'Connected',
  searching: 'Signal lost — searching…',
  disconnected: 'Disconnected'
}

const ANT_HELP: Record<string, string> = {
  win32:
    'Windows: the stick needs the generic WinUSB driver. Run Zadig (zadig.akeo.ie), choose the ANT stick and install “WinUSB”. Close Garmin Express and other training apps first.',
  linux: 'Linux: install the bundled udev rule (resources/99-ant-usb.rules) so the stick can be opened without root, then replug it.',
  darwin: 'macOS: quit Garmin Express and any other app using the stick — only one program can hold it at a time.'
}

export function Devices(): React.JSX.Element {
  const { devices, ant } = useDevices()
  const [scanning, setScanning] = useState(false)
  const [connecting, setConnecting] = useState('')
  const [candidates, setCandidates] = useState<BtCandidate[]>([])
  const [error, setError] = useState('')

  // Live readings change several times a second; refresh the list at a calmer rate.
  useEffect(() => {
    const timer = setInterval(() => void refreshDevices(), 500)
    return () => clearInterval(timer)
  }, [])

  useEffect(() => api.devices.onCandidates(setCandidates), [])
  // Don't leave the radio scanning if the screen is closed mid-search.
  useEffect(() => () => void api.devices.scanStop(), [])

  const scan = async (): Promise<void> => {
    setError('')
    setCandidates([])
    try {
      await api.devices.scanStart()
      setScanning(true)
    } catch (err) {
      setError(errorText(err))
    }
  }

  const stopScan = (): void => {
    setScanning(false)
    void api.devices.scanStop()
  }

  const connect = async (c: BtCandidate): Promise<void> => {
    setConnecting(c.id)
    try {
      await api.devices.connect(c.id)
      setScanning(false)
    } catch (err) {
      setError(`${c.name || 'Sensor'}: ${errorText(err)}`)
      stopScan()
    } finally {
      setConnecting('')
    }
  }

  const antDevices = devices.filter((d) => d.transport === 'ant').length

  return (
    <div>
      <div className="page-head">
        <div>
          <h1>Devices</h1>
          <p className="muted">
            Connect a smart trainer for automatic gradient resistance, plus any power meter, heart-rate strap or cadence sensor. When several
            devices report the same thing, the most direct measurement is used.
          </p>
        </div>
      </div>

      <div className="row wrap">
        <button className="primary" disabled={scanning} onClick={() => void scan()}>
          Add Bluetooth device…
        </button>
        {ant.state === 'off' || ant.state === 'error' ? (
          <button className="primary" onClick={() => void api.devices.antStart()}>
            Start ANT+ search
          </button>
        ) : (
          <button onClick={() => void api.devices.antStop()}>Stop ANT+</button>
        )}
        <button onClick={() => void api.devices.demoAdd()}>Add demo rider</button>
      </div>

      {error && <div className="error">{error}</div>}
      {ant.state === 'opening' && <div className="notice">Opening ANT+ stick…</div>}
      {ant.state === 'scanning' && (
        <div className="notice">
          ANT+ stick active — {antDevices ? `${antDevices} sensor${antDevices > 1 ? 's' : ''} found, still ` : ''}listening for trainers, power
          meters, heart-rate, speed and cadence sensors. Wake your sensors (spin the cranks, wear the strap) and they will appear below.
        </div>
      )}
      {ant.state === 'error' && (
        <div className="error">
          {ant.message}
          <div className="muted small">{ANT_HELP[platform]}</div>
        </div>
      )}

      <div className="list">
        {devices.length === 0 && <div className="empty">Nothing connected yet.</div>}
        {devices.map((d) => {
          const r = d.readings
          return (
            <div className="card device" key={d.id}>
              <div>
                <h3>{d.name}</h3>
                <div className="muted small">
                  {KIND_LABEL[d.kind]} · {d.transport === 'ble' ? 'Bluetooth' : d.transport === 'ant' ? 'ANT+' : 'Simulated'}
                  {d.kind === 'trainer' && (d.controllable ? ' · controllable' : ' · read-only')}
                </div>
              </div>
              <div className="readings">
                {r.power !== null && <span>{Math.round(r.power)} W</span>}
                {r.cadence !== null && <span>{Math.round(r.cadence)} rpm</span>}
                {r.hr !== null && <span>♥ {Math.round(r.hr)}</span>}
                {r.wheelSpeed !== null && <span>{(r.wheelSpeed * 3.6).toFixed(1)} km/h</span>}
              </div>
              <div className={`status ${d.status}`}>{STATUS_LABEL[d.status]}</div>
              <button className="ghost" onClick={() => void api.devices.remove(d.id)}>
                Remove
              </button>
            </div>
          )
        })}
      </div>

      <div className="card help">
        <h3>Using an Apple Watch as your heart-rate monitor</h3>
        <p className="muted">
          Apple Watch can't broadcast heart rate to a computer by itself. Install a broadcaster app on the watch (for example HeartCast or
          BlueHeart) and it will show up here under “Add Bluetooth device” like any chest strap.
        </p>
      </div>

      {scanning && (
        <div className="modal-backdrop">
          <div className="modal">
            <h2>Searching for Bluetooth sensors…</h2>
            <p className="muted small">Make sure the sensor is awake and not connected to another app or head unit.</p>
            <div className="picker">
              {candidates.length === 0 && <div className="empty">Nothing found yet</div>}
              {candidates.map((c) => (
                <button key={c.id} disabled={!!connecting} onClick={() => void connect(c)}>
                  {connecting === c.id ? `Connecting to ${c.name || 'sensor'}…` : c.name || 'Unnamed sensor'}
                </button>
              ))}
            </div>
            <button className="ghost" disabled={!!connecting} onClick={stopScan}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
