import { createRoot } from 'react-dom/client'
import { App } from './App'
import { api } from './bridge'
import './styles.css'

// Surface UI errors in the terminal the app was started from.
window.addEventListener('error', (e) => void api.log(`${e.message} (${e.filename}:${e.lineno})`))
window.addEventListener('unhandledrejection', (e) => void api.log(`unhandled rejection: ${e.reason}`))

createRoot(document.getElementById('root')!).render(<App />)
