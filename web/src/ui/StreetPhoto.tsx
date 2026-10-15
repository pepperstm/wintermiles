import { useEffect, useRef, useState } from 'react'
import { errorText } from '../bridge'
import { api } from '../state'

interface Photo {
  distance: number
  id: string
  url: string
  capturedAt: number
}

/** The nearest street-level photo just ahead, as a picture-in-picture. */
export function StreetPhoto({ distance }: { distance: number }): React.JSX.Element | null {
  const [photos, setPhotos] = useState<Photo[] | null>(null)
  const [error, setError] = useState('')
  const [shown, setShown] = useState<{ id: string; src: string; capturedAt: number } | null>(null)
  const loading = useRef('')
  useEffect(() => {
    void api.photos.forRide().then(setPhotos, (err) => setError(errorText(err)))
  }, [])

  // The last photo at or just behind the rider, if it is close enough to be this stretch of road.
  let current: Photo | null = null
  for (const p of photos ?? []) {
    if (p.distance > distance + 15) break
    current = p
  }
  if (current && distance - current.distance > 80) current = null

  useEffect(() => {
    if (!current || current.id === shown?.id || loading.current === current.id) return
    const p = current
    loading.current = p.id
    void api.photos.image(p.url).then(
      (src) => setShown({ id: p.id, src, capturedAt: p.capturedAt }),
      () => undefined
    )
  }, [current, shown?.id])

  if (error) return <div className="street-photo empty">{error}</div>
  if (!photos) return <div className="street-photo empty">Looking for street-level photos…</div>
  if (!photos.length) return null
  if (!shown || !current) return <div className="street-photo empty">No photo of this stretch</div>
  return (
    <div className="street-photo">
      <img src={shown.src} alt="Street-level photo of the road here" />
      <span>
        Mapillary · {new Date(shown.capturedAt).toLocaleDateString(undefined, { year: 'numeric', month: 'short' })}
      </span>
    </div>
  )
}
