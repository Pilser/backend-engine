import { useEffect, useState } from 'react'
import { api, show, getKey, setKey } from '../api.js'

export default function Access({ onKey }) {
  const [keys, setKeys] = useState([])
  const [role, setRole] = useState('reader')
  const [email, setEmail] = useState('demo@example.com')
  const [pass, setPass] = useState('')
  const [out, setOut] = useState('')

  async function load() {
    const r = await api('/api/keys')
    if (r.ok) setKeys(r.data.keys ?? (Array.isArray(r.data) ? r.data : []))
    else setOut(show(r.data))
  }
  useEffect(() => { load() }, [])
  const run = async (label, fn) => {
    try { const r = await fn(); setOut(`${label} → HTTP ${r.status}\n${show(r.data)}`); load(); return r }
    catch (e) { setOut(`${label} failed: ${e.message}`) }
  }

  return (
    <section>
      <h2>Access (keys + users)</h2>
      <p>Page key in use: <code>{getKey() ? getKey().slice(0, 8) + '…' : '(none — public reads only)'}</code></p>
      <h3>API keys ({keys.length})</h3>
      <ul>{keys.map((k) => (
        <li key={k.name ?? k.bucket}>{k.role} <code>{((k.bucket ?? k.name ?? '')).slice(0, 8)}</code>{' '}
          <button onClick={() => run('revoke', () => api(`/api/keys?bucket=${encodeURIComponent(k.bucket ?? k.name)}`, { method: 'DELETE' }))}>revoke</button>
        </li>))}
      </ul>
      <div>
        <select value={role} onChange={(e) => setRole(e.target.value)}>
          <option>reader</option><option>writer</option><option>customer</option>
        </select>
        <button onClick={() => run('issue key (secret shown ONCE below)', () => api('/api/keys', { method: 'POST', body: { role } }))}>issue key</button>
      </div>
      <h3>Users</h3>
      <div>
        <input value={email} onChange={(e) => setEmail(e.target.value)} placeholder="email" />
        <input type="password" value={pass} onChange={(e) => setPass(e.target.value)} placeholder="password" />
        <button onClick={() => run('signup', () => api('/api/auth/signup', { method: 'POST', body: { email, password: pass, role: 'reader' } }))}>signup</button>
        <button onClick={async () => {
          const r = await run('login', () => api('/api/auth/login', { method: 'POST', body: { email, password: pass } }))
          if (r?.ok && r.data.token) { setKey(r.data.token); onKey?.(); setOut(`login ok — session token now in use`) }
        }}>login + use session</button>
        <button onClick={() => run('me', () => api('/api/auth/me'))}>me</button>
      </div>
      <pre>{out}</pre>
    </section>
  )
}
