import { useState } from 'react'
import { api, show } from '../api.js'

// The third door: the same CLI grammar agents use, runnable from here.
// (?command= works for GET; POST carries {"command"} as JSON.)
const QUICK = ['--help', 'tables list', 'records list notes', 'jobs list', 'apps --help']

export default function Agent() {
  const [cmd, setCmd] = useState('records list notes')
  const [out, setOut] = useState('')

  async function send(c) {
    setOut('…')
    try {
      const r = await api('/mcp', { method: 'POST', body: { command: c } })
      setOut(`$ ${c}\nHTTP ${r.status}\n${show(r.data)}`)
    } catch (e) { setOut(`failed: ${e.message}`) }
  }

  return (
    <section>
      <h2>Agent console (the /mcp door)</h2>
      <p>One tool (<code>manage_serverless_engine</code>) speaks CLI. Type any
      command — this page POSTs <code>{'{"command":"…"}\u2009'}</code> to <code>/mcp</code>,
      exactly like an MCP client would.</p>
      <div>
        {QUICK.map((q) => <button key={q} onClick={() => { setCmd(q); send(q) }}>{q}</button>)}
      </div>
      <div>
        <input size={50} value={cmd} onChange={(e) => setCmd(e.target.value)} />
        <button onClick={() => send(cmd)}>run</button>
      </div>
      <pre>{out}</pre>
    </section>
  )
}
