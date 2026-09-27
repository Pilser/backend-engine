import { useEffect, useState } from 'react'
import { api, show, withKey } from '../api.js'

export default function Files() {
  const [files, setFiles] = useState([])
  const [out, setOut] = useState('')

  async function load() {
    await api('/api/tables', { method: 'POST', body: { table: 'files' } }) // ensure, ignore exists-error
    const r = await api('/api/tables/files/records')
    if (r.ok) setFiles(r.data.records ?? [])
    else setOut(show(r.data))
  }
  useEffect(() => { load() }, [])

  async function upload(e) {
    const f = e.target.files?.[0]
    if (!f) return
    const r = await api('/api/upload?table=files', {
      method: 'POST',
      headers: { 'X-Filename': f.name, 'Content-Type': f.type || 'application/octet-stream' },
      body: f,
    })
    setOut(`upload ${f.name} → HTTP ${r.status}\n${show(r.data)}`)
    if (r.ok) load()
  }

  return (
    <section>
      <h2>Files (R2 blobs + records)</h2>
      <input type="file" onChange={upload} />
      <ul>
        {files.map((r) => (
          <li key={r.seq}>
            #{r.seq} {r.payload?.name} ({r.payload?.size}b) —{' '}
            <a href={withKey(`/api/file?table=files&file=${encodeURIComponent(r.payload?.file ?? '')}`)} target="_blank" rel="noreferrer">download</a>
          </li>
        ))}
      </ul>
      <pre>{out}</pre>
    </section>
  )
}
