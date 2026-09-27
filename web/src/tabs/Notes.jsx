import { useEffect, useState } from 'react'
import { api, show } from '../api.js'

const norm = (t) => (typeof t === 'string' ? t : t?.table)

export default function Notes() {
  const [tables, setTables] = useState([])
  const [table, setTable] = useState('notes')
  const [newTable, setNewTable] = useState('')
  const [records, setRecords] = useState([])
  const [doc, setDoc] = useState('{"body":"hello from the SPA"}')
  const [filter, setFilter] = useState('{"body":{"search":"hello"}}')
  const [seq, setSeq] = useState('')
  const [out, setOut] = useState('')

  async function loadTables() {
    const r = await api('/api/tables')
    if (r.ok) setTables((r.data.tables ?? []).map(norm))
    else setOut(show(r.data))
  }
  async function loadRecords(q) {
    const r = await api(`/api/tables/${table}/records${q ?? ''}`)
    if (r.ok) setRecords(r.data.records ?? [])
    else setOut(show(r.data))
  }
  useEffect(() => { loadTables() }, [])
  useEffect(() => { if (table) loadRecords() }, [table])

  const run = async (label, fn) => {
    try { const r = await fn(); setOut(`${label} → HTTP ${r.status}\n${show(r.data)}`); return r }
    catch (e) { setOut(`${label} failed: ${e.message}`) }
  }

  return (
    <section>
      <h2>Tables + records</h2>
      <div>
        <select value={table} onChange={(e) => setTable(e.target.value)}>
          {tables.map((t) => <option key={t} value={t}>{t}</option>)}
        </select>
        <button onClick={() => loadTables()}>refresh</button>
        <button onClick={() => run('show table', () => api(`/api/tables/${table}`))}>config</button>
        <button onClick={async () => { await run('delete table', () => api(`/api/tables/${table}`, { method: 'DELETE' })); loadTables() }}>delete table</button>
      </div>
      <div>
        <input value={newTable} onChange={(e) => setNewTable(e.target.value)} placeholder="new table name" />
        <button onClick={async () => { const r = await run('create table', () => api('/api/tables', { method: 'POST', body: { table: newTable } })); if (r?.ok) { setTable(newTable); setNewTable(''); loadTables() } }}>create</button>
      </div>
      <h3>Submit (JSON)</h3>
      <textarea rows={3} cols={60} value={doc} onChange={(e) => setDoc(e.target.value)} />
      <br />
      <button onClick={async () => { const r = await run('submit', () => api(`/api/tables/${table}/submit`, { method: 'POST', body: doc })); if (r?.ok) loadRecords() }}>submit</button>
      <h3>Query (filter JSON → ?filter=)</h3>
      <input size={55} value={filter} onChange={(e) => setFilter(e.target.value)} />
      <button onClick={() => loadRecords(`?filter=${encodeURIComponent(filter)}`)}>run</button>
      <button onClick={() => loadRecords()}>clear</button>
      <h3>Records ({records.length})</h3>
      <ul>
        {records.map((r) => (
          <li key={r.seq}>#{r.seq} {show(r.payload)}</li>
        ))}
      </ul>
      <div>
        <input size={6} value={seq} onChange={(e) => setSeq(e.target.value)} placeholder="seq" />
        <button onClick={async () => { const r = await run('patch', () => api(`/api/tables/${table}/records/${seq}`, { method: 'PATCH', body: JSON.parse(doc) })); if (r?.ok) loadRecords() }}>patch seq with JSON above</button>
        <button onClick={async () => { const r = await run('delete', () => api(`/api/tables/${table}/records/${seq}`, { method: 'DELETE' })); if (r?.ok) loadRecords() }}>delete seq</button>
      </div>
      <pre>{out}</pre>
    </section>
  )
}
