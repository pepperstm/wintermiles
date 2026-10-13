import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  root: 'web',
  plugins: [react()],
  // Keep Rust compiler output visible when running under `tauri dev`.
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { outDir: '../dist-web', emptyOutDir: true, target: 'safari15', chunkSizeWarningLimit: 2500 }
})
