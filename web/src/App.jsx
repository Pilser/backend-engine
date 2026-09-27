import { useEffect, useState } from 'react'
import { api, show, getKey, setKey } from './api.js'
import Notes from './tabs/Notes.jsx'
import Files from './tabs/Files.jsx'
import Automate from './tabs/Automate.jsx'
import Access from './tabs/Access.jsx'
import Agent from './tabs/Agent.jsx'

const TABS = { status: 'Status', notes: 'Tables', files: 'Files', automate: 'Automate', access: 'Access', agent: 'Agent' }

export default function App() {
  const [tab, setTab] = useState('status')
  const [info, setInfo] = useState(null)
  const [key, setKeyState] = useState(getKey())

  async function refresh() {
    const [app, res, health] = await Promise.all([
      api('/api/app'), api('/api/resources'),
      fetch('/api/system/health').then((r) => r.json()).catch((e) => ({ error: e.message })),
    ])
    setInfo({ app, res, health })
  }
  useEffect(() => { refresh() }, [])

  return (
    <main style={{ fontFamily: 'system-ui', maxWidth: 760, margin: '2rem auto', padding: '0 1rem' }}>
      <h1>⚡ backend-engine demo</h1>
      <p>Your backend that you never code — every tab below is JSON over HTTP,
      no backend code exists for any of it.</p>
      <div>
        {Object.entries(TABS).map(([k, v]) => (
          <button key={k} disabled={k === tab} onClick={() => setTab(k)}>{v}</button>
        ))}
      </div>
      <div>
        <input size={40} value={key} onChange={(e) => { setKey(e.target.value); setKeyState(e.target.value) }} placeholder="page key (?key=)" />
        <button onClick={refresh}>reload status</button>
      </div>
      {tab === 'status' && (
        <section>
          <h2>Status</h2>
          <h3>GET /api/app → {info?.app.status}</h3><pre>{show(info?.app.data)}</pre>
          <h3>GET /api/resources → {info?.res.status}</h3><pre>{show(info?.res.data)}</pre>
          <h3>GET /api/system/health</h3><pre>{show(info?.health)}</pre>
        </section>
      )}
      {tab === 'notes' && <Notes />}
      {tab === 'files' && <Files />}
      {tab === 'automate' && <Automate />}
      {tab === 'access' && <Access onKey={() => setKeyState(getKey())} />}
      {tab === 'agent' && <Agent />}
    </main>
  )
}
