export type Units = 'metric' | 'imperial'

export function fmtTime(s: number): string {
  s = Math.max(0, Math.floor(s))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  const mm = h ? String(m).padStart(2, '0') : String(m)
  return `${h ? `${h}:` : ''}${mm}:${String(sec).padStart(2, '0')}`
}

export function fmtDist(m: number, units: Units, digits = 1): string {
  return units === 'imperial' ? `${(m / 1609.344).toFixed(digits)} mi` : `${(m / 1000).toFixed(digits)} km`
}

export function distValue(m: number, units: Units): number {
  return units === 'imperial' ? m / 1609.344 : m / 1000
}

export function distUnit(units: Units): string {
  return units === 'imperial' ? 'mi' : 'km'
}

export function speedValue(ms: number, units: Units): number {
  return units === 'imperial' ? ms * 2.236936 : ms * 3.6
}

export function speedUnit(units: Units): string {
  return units === 'imperial' ? 'mph' : 'km/h'
}

export function fmtEle(m: number, units: Units): string {
  return units === 'imperial' ? `${Math.round(m * 3.28084)} ft` : `${Math.round(m)} m`
}

/** Coggan power zones 1–7 as a colour. */
export function zoneColor(power: number, ftp: number): string {
  const r = ftp > 0 ? power / ftp : 0
  if (r < 0.55) return '#8a94a6'
  if (r < 0.75) return '#4aa3ff'
  if (r < 0.9) return '#4ddb8a'
  if (r < 1.05) return '#ffd24a'
  if (r < 1.2) return '#ff9a3c'
  if (r < 1.5) return '#ff5a4a'
  return '#d44aff'
}

export function gradeColor(grade: number): string {
  const g = grade * 100
  if (g < -1) return '#4aa3ff'
  if (g < 2) return '#4ddb8a'
  if (g < 5) return '#ffd24a'
  if (g < 8) return '#ff9a3c'
  if (g < 12) return '#ff5a4a'
  return '#b0308a'
}

/** XP → level. Mirrors `level_for` in the Rust core. */
export function levelFor(xp: number): { level: number; into: number; span: number } {
  const level = Math.floor(Math.sqrt(xp / 200)) + 1
  const floor = 200 * (level - 1) ** 2
  return { level, into: xp - floor, span: 200 * level ** 2 - floor }
}
