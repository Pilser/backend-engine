import { useEffect, useState } from 'react'
import { api, show } from '../api.js'

export default function Automate() {
  const [recipes, setRecipes] = useState([])
  const [jobs, setJobs] = useState([])
  const [runs, setRuns] = useState([])
  const [out, setOut] = useState('')
  const [rName, setRName] = useState('note_logger')
  const [jName, setJName] = useState('demo-heartbeat')
  const [jSched, setJSched] = useState('@every 5m')

  async function load() {
    const [rr, jj, rn] = await Promise.all([api('/api/recipes'), api('/api/jobs'), api('/api/jobs/runs')])
    const arr = (d) => (Array.isArray(d) ? d : [])
    if (rr.ok) setRecipes(rr.data.recipes ?? arr(rr.data))
    if (jj.ok) setJobs(jj.data.jobs ?? arr(jj.data))
    if (rn.ok) setRuns(rn.data.runs ?? arr(rn.data))
    setOut(show({ recipes: rr.status, jobs: jj.status, runs: rn.status }))
  }
  useEffect(() => { load() }, [])
  const run = async (label, fn) => {
    try { const r = await fn(); setOut(`${label} → HTTP ${r.status}\n${show(r.data)}`); load(); return r }
    catch (e) { setOut(`${label} failed: ${e.message}`) }
  }

  return (
    <section>
      <h2>Automations (recipes) + cron jobs</h2>
      <h3>Recipes ({recipes.length})</h3>
      <ul>{recipes.map((r) => (
        <li key={r.name}>{r.name} on {r.when_json?.event} {r.when_json?.table ?? ''}{' '}
          <button onClick={() => run('delete recipe', () => api(`/api/recipes/${r.name}`, { method: 'DELETE' }))}>delete</button>
        </li>))}
      </ul>
      <div>
        <input value={rName} onChange={(e) => setRName(e.target.value)} placeholder="recipe name" />
        <button onClick={() => run('add recipe', () => api('/api/recipes', { method: 'POST', body: {
          name: rName, when: { event: 'record.created', table: 'notes' }, actions: [{ $log: 'a note landed' }],
        } }))}>log every new note</button>
      </div>
      <h3>Jobs ({jobs.length})</h3>
      <ul>{jobs.map((j) => (
        <li key={j.name}>{j.name} <code>{j.schedule}</code> next {j.next_run_at ?? '—'}{' '}
          <button onClick={() => run('delete job', () => api(`/api/jobs?name=${encodeURIComponent(j.name)}`, { method: 'DELETE' }))}>delete</button>
        </li>))}
      </ul>
      <div>
        <input value={jName} onChange={(e) => setJName(e.target.value)} placeholder="job name" />
        <input value={jSched} onChange={(e) => setJSched(e.target.value)} placeholder="schedule" />
        <button onClick={() => run('add job', () => api('/api/jobs', { method: 'POST', body: {
          name: jName, schedule: jSched, action: { type: 'http', url: 'https://example.com/ping' },
        } }))}>add ping job</button>
      </div>
      <h3>Recent runs ({runs.length})</h3>
      <pre>{show(runs.slice(0, 5))}</pre>
      <pre>{out}</pre>
    </section>
  )
}
