// Tiny JSON-over-HTTP client for backend-engine.
// Auth: ?key= carries WORKER_KEY, API keys, or login session tokens.
export const ENGINE_URL = import.meta.env.VITE_ENGINE_URL ?? ''

let apiKey = import.meta.env.VITE_API_KEY ?? ''
export const getKey = () => apiKey
export const setKey = (k) => { apiKey = k ?? '' }

export function withKey(path) {
  const sep = path.includes('?') ? '&' : '?'
  return `${ENGINE_URL}${path}${sep}key=${encodeURIComponent(apiKey)}`
}

export async function api(path, opts = {}) {
  const { method = 'GET', body, headers = {} } = opts
  const isBlob = body instanceof Blob
  const res = await fetch(withKey(path), {
    method,
    headers: isBlob ? headers : { 'Content-Type': 'application/json', ...headers },
    body: body === undefined ? undefined : (typeof body === 'string' || isBlob ? body : JSON.stringify(body)),
  })
  const text = await res.text()
  let data
  try { data = JSON.parse(text) } catch { data = text }
  return { status: res.status, ok: res.ok, data }
}

export const show = (v) => {
  const s = typeof v === 'string' ? v : JSON.stringify(v, null, 1)
  return (s ?? '').slice(0, 1500)
}
