import { useState } from 'react'
import { ApiError } from '../api'

export function useConfigWrite() {
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  async function withWrite<T>(action: () => Promise<T>) {
    setSaving(true)
    setError(null)
    setNotice(null)
    try {
      const result = await action()
      setNotice('已保存')
      return result
    } catch (reason: unknown) {
      setError(reason instanceof ApiError ? reason.message : String(reason))
    } finally {
      setSaving(false)
    }
  }

  return { saving, error, notice, setError, setNotice, withWrite }
}
