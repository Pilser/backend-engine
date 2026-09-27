import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// base './' keeps asset URLs RELATIVE so the engine's /srv/ hosting
// (which injects <base href="/srv/">) serves the SPA from any path.
export default defineConfig({
  plugins: [react()],
  base: './',
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://localhost:8788',
      '/mcp': 'http://localhost:8788',
      '/srv': 'http://localhost:8788',
    },
  },
})
