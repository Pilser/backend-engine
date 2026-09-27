import { useEffect, useState } from 'react'

// Same origin when served from the engine (/srv/); override for `vite dev`.
const ENGINE_URL = import.meta.env.VITE_ENGINE_URL ?? ''
const API_KEY = import.meta.env.VITE_API_KEY ?? ''

async function api(path, options = {}) {
  const sep = path.includes('?') ? '&' : '?'
  const res = await fetch(`${ENGINE_URL}${path}${sep}key=${API_KEY}`, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  })
  return res.json()
}

export default function App() {
  const [notes, setNotes] = useState([])
  const [body, setBody] = useState('')
  const [status, setStatus] = useState('connecting…')

  async function refresh() {
    try {
      // The table IS the schema — creating it is one JSON call, no migration.
      await api('/api/tables', { method: 'POST', body: JSON.stringify({ table: 'notes' }) })
      const data = await api('/api/tables/notes/records')
      setNotes(data.records ?? [])
      setStatus('connected')
    } catch (e) {
      setStatus(`engine unreachable (${e.message}) — is wrangler dev running?`)
    }
  }

  useEffect(() => { refresh() }, [])

  async function add(e) {
    e.preventDefault()
    if (!body.trim()) return
    await api('/api/tables/notes/submit', { method: 'POST', body: JSON.stringify({ body }) })
    setBody('')
    refresh()
  }

  return (
    <main style={{ fontFamily: 'system-ui', maxWidth: 560, margin: '2rem auto' }}>
      <h1>⚡ backend-engine demo</h1>
      <p>
        Your backend that you never code. This page is static files in the
        engine's asset store (<code>/srv/</code>) — every word below it comes
        from JSON over HTTP. Status: <b>{status}</b>
      </p>
      <form onSubmit={add}>
        <input value={body} onChange={(e) => setBody(e.target.value)} placeholder="new note" />
        <button type="submit">add</button>
      </form>
      <ul>
        {notes.map((n) => (
          <li key={n.seq}>{n.payload?.body}</li>
        ))}
      </ul>
    </main>
  )
}
