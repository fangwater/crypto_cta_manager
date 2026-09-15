import { Shield, UserRound } from 'lucide-react'
import { useEffect, useState } from 'react'
import {
  listAuthUsers,
  setAuthUserRole,
  setAuthUserSources,
  type AuthUser,
  getDashboard,
} from '../api'
import { AppShell, PageIntro } from '../components/AppShell'
import { useAuth } from '../components/AuthGate'
import { Alert } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/Card'
import { routes } from '../lib/routes'

export function AdminPage() {
  const { user } = useAuth()
  const [users, setUsers] = useState<AuthUser[]>([])
  const [sources, setSources] = useState<Array<{ source_id: string; account: string }>>([])
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState<number | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    Promise.all([listAuthUsers(controller.signal), getDashboard(controller.signal)])
      .then(([nextUsers, dashboard]) => {
        setUsers(nextUsers)
        setSources((dashboard.accounts ?? []).map((account) => ({ source_id: account.source_id, account: account.account })))
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => controller.abort()
  }, [])

  async function saveSources(target: AuthUser, sourceIds: string[]) {
    setSaving(target.user_id)
    try {
      const updated = await setAuthUserSources(target.user_id, sourceIds)
      setUsers((current) => current.map((item) => item.user_id === updated.user_id ? updated : item))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSaving(null)
    }
  }

  async function toggleRole(target: AuthUser) {
    setSaving(target.user_id)
    try {
      const updated = await setAuthUserRole(target.user_id, target.role === 'admin' ? 'user' : 'admin')
      setUsers((current) => current.map((item) => item.user_id === updated.user_id ? updated : item))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSaving(null)
    }
  }

  return (
    <AppShell active="admin" title="权限管理" subtitle="用户与账户可见范围" icon={Shield}>
      <PageIntro eyebrow="Access Control" title="账户权限" description="管理员为普通用户选择可见账户；未授权账户不会出现在总览、净值、成本或账户详情中。" />
      {error && <Alert tone="error" className="mb-4">{error}</Alert>}
      <Card>
        <CardHeader><CardTitle>已注册用户</CardTitle></CardHeader>
        <CardContent className="space-y-5">
          {users.map((target) => (
            <div key={target.user_id} className="rounded-xl border border-border-soft p-4">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="flex items-center gap-3">
                  <div className="grid h-9 w-9 place-items-center rounded-lg bg-canvas text-muted"><UserRound size={17} /></div>
                  <div><p className="text-sm font-medium text-ink">{target.username}</p><p className="text-xs text-muted">{target.role === 'admin' ? '管理员' : '普通用户'}{target.user_id === user.user_id ? ' · 当前账号' : ''}</p></div>
                </div>
                {target.user_id !== user.user_id && <Button size="sm" disabled={saving === target.user_id} onClick={() => void toggleRole(target)}>{target.role === 'admin' ? '降为普通用户' : '设为管理员'}</Button>}
              </div>
              {target.role !== 'admin' && (
                <div className="mt-4 flex flex-wrap gap-2">
                  {sources.map((source) => {
                    const checked = target.source_ids.includes(source.source_id)
                    return <label key={source.source_id} className="flex cursor-pointer items-center gap-2 rounded-lg border border-border-soft px-3 py-2 text-xs text-muted hover:border-brand-ring"><input type="checkbox" checked={checked} disabled={saving === target.user_id} onChange={() => void saveSources(target, checked ? target.source_ids.filter((id) => id !== source.source_id) : [...target.source_ids, source.source_id])} /><span>{source.account}</span><span className="font-mono text-[10px] text-subtle">{source.source_id}</span></label>
                  })}
                </div>
              )}
            </div>
          ))}
          {!users.length && <p className="text-sm text-muted">暂无用户</p>}
        </CardContent>
      </Card>
      <a className="mt-5 inline-flex text-sm font-medium text-brand hover:text-brand-hover" href={routes.workspace}>返回总览</a>
    </AppShell>
  )
}
