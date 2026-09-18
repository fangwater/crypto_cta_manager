import { Shield, UserRound } from 'lucide-react'
import { useEffect, useState } from 'react'
import {
  addPublishToken,
  createAuthUser,
  deletePublishToken,
  listAccountGrants,
  listAuthUsers,
  listPositionAccess,
  listPublishTokens,
  resetPositionPublishToken,
  setAccountGrants as saveAccountGrantsApi,
  setAuthUserRole,
  setAuthUserSources,
  setPositionManagers,
  setPositionGrants,
  setPositionPublishToken,
  type AccountGrant,
  type AuthUser,
  type PositionAccess,
  type PublishToken,
  type SourceGrant,
  getDashboard,
} from '../api'
import { AppShell, PageIntro } from '../components/AppShell'
import { useAuth } from '../components/AuthGate'
import { Alert } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/Card'
import { routes } from '../lib/routes'
import { cn } from '../lib/cn'

type AccessLevel = 'view' | 'configure'

const levelOptions: Array<{ value: AccessLevel | 'none'; label: string }> = [
  { value: 'none', label: '无权限' },
  { value: 'view', label: '仅查看' },
  { value: 'configure', label: '可配置' },
]

function LevelPicker({
  value,
  disabled,
  onChange,
}: {
  value: AccessLevel | 'none'
  disabled: boolean
  onChange: (next: AccessLevel | 'none') => void
}) {
  return (
    <div className="inline-flex overflow-hidden rounded-lg border border-border-soft">
      {levelOptions.map((option) => (
        <button
          key={option.value}
          type="button"
          disabled={disabled}
          onClick={() => onChange(option.value)}
          className={cn(
            'px-2.5 py-1.5 text-[11px] transition-colors disabled:opacity-50',
            value === option.value
              ? 'bg-brand text-white'
              : 'bg-canvas text-muted hover:text-ink',
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  )
}

export function AdminPage() {
  const { user } = useAuth()
  const isAdmin = user.role === 'admin'
  const [users, setUsers] = useState<AuthUser[]>([])
  const [sources, setSources] = useState<
    Array<{ source_id: string; account: string; access_level?: 'view' | 'configure' | null }>
  >([])
  const [accountGrants, setAccountGrants] = useState<Record<string, AccountGrant[]>>({})
  const [access, setAccess] = useState<PositionAccess[]>([])
  const [fallbackTokens, setFallbackTokens] = useState<PublishToken[]>([])
  const [tokenDrafts, setTokenDrafts] = useState<Record<string, string>>({})
  const [revealedTokens, setRevealedTokens] = useState<Record<string, string>>({})
  const [newUsername, setNewUsername] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [fallbackNote, setFallbackNote] = useState('')
  const [fallbackDraft, setFallbackDraft] = useState('')
  const [revealedFallback, setRevealedFallback] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState<number | null>(null)
  const [savingAccount, setSavingAccount] = useState<string | null>(null)
  const [savingStrategy, setSavingStrategy] = useState<string | null>(null)
  const [savingFallback, setSavingFallback] = useState(false)
  const [creatingUser, setCreatingUser] = useState(false)

  useEffect(() => {
    const controller = new AbortController()
    const jobs: Array<Promise<void>> = [
      listAuthUsers(controller.signal).then(setUsers),
      listPositionAccess(controller.signal).then(setAccess),
      getDashboard(controller.signal).then(async (dashboard) => {
        const accounts = (dashboard.accounts ?? []).map((account) => ({
          source_id: account.source_id,
          account: account.account,
          access_level: account.access_level,
        }))
        setSources(accounts)
        // Configure holders manage their accounts' grant lists too.
        const entries = await Promise.all(
          accounts
            .filter((account) => account.access_level === 'configure')
            .map(async (account) => [
              account.source_id,
              await listAccountGrants(account.source_id, controller.signal),
            ] as const),
        )
        setAccountGrants(Object.fromEntries(entries))
      }),
    ]
    if (isAdmin) {
      jobs.push(listPublishTokens(controller.signal).then(setFallbackTokens))
    }
    Promise.all(jobs).catch((reason: unknown) => {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setError(reason instanceof Error ? reason.message : String(reason))
    })
    return () => controller.abort()
  }, [isAdmin])

  async function createUser() {
    const username = newUsername.trim()
    if (!username || !newPassword) return
    setCreatingUser(true)
    try {
      const created = await createAuthUser(username, newPassword)
      setUsers((current) => [...current, created])
      setNewUsername('')
      setNewPassword('')
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setCreatingUser(false)
    }
  }

  async function saveSourceLevel(target: AuthUser, sourceId: string, level: AccessLevel | 'none') {
    const grants: SourceGrant[] = target.source_grants.filter((grant) => grant.source_id !== sourceId)
    if (level !== 'none') grants.push({ source_id: sourceId, access_level: level })
    setSaving(target.user_id)
    try {
      const updated = await setAuthUserSources(target.user_id, grants)
      setUsers((current) => current.map((item) => item.user_id === updated.user_id ? updated : item))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSaving(null)
    }
  }

  async function saveAccountGrantLevel(sourceId: string, target: AuthUser, level: AccessLevel | 'none') {
    const grants = (accountGrants[sourceId] ?? [])
      .filter((grant) => grant.user_id !== target.user_id)
      .map((grant) => ({ user_id: grant.user_id, access_level: grant.access_level }))
    if (level !== 'none') grants.push({ user_id: target.user_id, access_level: level })
    setSavingAccount(sourceId)
    try {
      const updated = await saveAccountGrantsApi(sourceId, grants)
      setAccountGrants((current) => ({ ...current, [sourceId]: updated }))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingAccount(null)
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

  function replaceAccess(updated: PositionAccess) {
    setAccess((current) => current.map((item) => item.strategy_name === updated.strategy_name ? updated : item))
  }

  async function saveToken(item: PositionAccess) {
    const draft = (tokenDrafts[item.strategy_name] ?? '').trim()
    if (!draft) return
    setSavingStrategy(item.strategy_name)
    try {
      replaceAccess(await setPositionPublishToken(item.strategy_name, draft))
      setTokenDrafts((current) => ({ ...current, [item.strategy_name]: '' }))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingStrategy(null)
    }
  }

  async function clearToken(item: PositionAccess) {
    setSavingStrategy(item.strategy_name)
    try {
      replaceAccess(await setPositionPublishToken(item.strategy_name, ''))
      setRevealedTokens((current) => ({ ...current, [item.strategy_name]: '' }))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingStrategy(null)
    }
  }

  async function resetToken(item: PositionAccess) {
    setSavingStrategy(item.strategy_name)
    try {
      const result = await resetPositionPublishToken(item.strategy_name)
      replaceAccess(result.access)
      setRevealedTokens((current) => ({ ...current, [item.strategy_name]: result.publish_token }))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingStrategy(null)
    }
  }

  async function addFallback() {
    setSavingFallback(true)
    try {
      const created = await addPublishToken(fallbackNote.trim(), fallbackDraft.trim() || undefined)
      setFallbackTokens((current) => [...current, created])
      setRevealedFallback(created.publish_token)
      setFallbackNote('')
      setFallbackDraft('')
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingFallback(false)
    }
  }

  async function removeFallback(item: PublishToken) {
    setSavingFallback(true)
    try {
      await deletePublishToken(item.token_id)
      setFallbackTokens((current) => current.filter((token) => token.token_id !== item.token_id))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingFallback(false)
    }
  }

  async function toggleManager(item: PositionAccess, target: AuthUser, checked: boolean) {
    const next = checked
      ? [...item.managers.map((manager) => manager.user_id), target.user_id]
      : item.managers.filter((manager) => manager.user_id !== target.user_id).map((manager) => manager.user_id)
    setSavingStrategy(item.strategy_name)
    try {
      replaceAccess(await setPositionManagers(item.strategy_name, next))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingStrategy(null)
    }
  }

  async function saveGrants(item: PositionAccess, grants: Array<{ user_id: number; access_level: AccessLevel }>, openVisibility: boolean) {
    setSavingStrategy(item.strategy_name)
    try {
      replaceAccess(await setPositionGrants(item.strategy_name, grants, openVisibility))
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSavingStrategy(null)
    }
  }

  async function setUserGrantLevel(item: PositionAccess, target: AuthUser, level: AccessLevel | 'none') {
    const grants = item.grants
      .filter((grant) => grant.user_id !== target.user_id)
      .map((grant) => ({ user_id: grant.user_id, access_level: grant.access_level }))
    if (level !== 'none') grants.push({ user_id: target.user_id, access_level: level })
    await saveGrants(item, grants, item.open_visibility)
  }

  async function toggleOpen(item: PositionAccess, checked: boolean) {
    await saveGrants(
      item,
      item.grants.map((grant) => ({ user_id: grant.user_id, access_level: grant.access_level })),
      checked,
    )
  }

  const grantableUsers = users.filter((target) => target.role !== 'admin')

  return (
    <AppShell active="admin" title="权限管理" subtitle={isAdmin ? '用户、账户与策略权限' : '授权管理'} icon={Shield}>
      <PageIntro
        eyebrow="Access Control"
        title={isAdmin ? '账户权限' : '授权管理'}
        description={isAdmin
          ? '账户授权分三档：无权限（不显示）、仅查看（可读账户与绑定）、可配置（可编辑绑定、份数、执行参数、费率与杠杆）。策略授权独立于账户：可见即可在目录与已绑定账户中看到该策略，可配置才允许新建绑定并管理该策略的授权。已存在的绑定由账户可配置权限管理，不再重复校验策略权限。'
          : '你持有可配置权限的账户和策略都可以继续授权给其他用户：账户的可配置授权允许对方读写该账户绑定与费率，策略的可配置授权允许对方新建绑定并继续授权。全员可见、推送 token、推送用户与新建策略仍由管理员维护。'}
      />
      {error && <Alert tone="error" className="mb-4">{error}</Alert>}
      {isAdmin && (
        <Card>
          <CardHeader><CardTitle>已注册用户</CardTitle></CardHeader>
          <CardContent className="space-y-5">
            <div className="flex flex-wrap items-center gap-2">
              <input
                type="text"
                placeholder="新用户名（3-64 字符）"
                value={newUsername}
                disabled={creatingUser}
                onChange={(event) => setNewUsername(event.target.value)}
                className="h-8 w-48 rounded-lg border border-border-soft bg-canvas px-3 text-xs text-ink"
              />
              <input
                type="password"
                placeholder="初始密码（至少 8 位）"
                value={newPassword}
                disabled={creatingUser}
                onChange={(event) => setNewPassword(event.target.value)}
                className="h-8 w-48 rounded-lg border border-border-soft bg-canvas px-3 text-xs text-ink"
              />
              <Button size="sm" disabled={creatingUser || !newUsername.trim() || !newPassword} onClick={() => void createUser()}>创建用户</Button>
            </div>
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
                  <div className="mt-4 space-y-2">
                    {sources.map((source) => {
                      const level = target.source_grants.find((grant) => grant.source_id === source.source_id)?.access_level ?? 'none'
                      return (
                        <div key={source.source_id} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border-soft px-3 py-2">
                          <div className="text-xs text-muted"><span className="text-ink">{source.account}</span> <span className="font-mono text-[10px] text-subtle">{source.source_id}</span></div>
                          <LevelPicker
                            value={level}
                            disabled={saving === target.user_id}
                            onChange={(next) => void saveSourceLevel(target, source.source_id, next)}
                          />
                        </div>
                      )
                    })}
                  </div>
                )}
              </div>
            ))}
            {!users.length && <p className="text-sm text-muted">暂无用户</p>}
          </CardContent>
        </Card>
      )}
      {!isAdmin && (
        <Card className="mt-5">
          <CardHeader><CardTitle>账户权限</CardTitle></CardHeader>
          <CardContent className="space-y-5">
            <p className="text-xs text-muted">你持有以下账户的可配置权限，可以为其他用户分配查看或可配置权限。查看即可读取账户与绑定，可配置允许对方编辑绑定、份数、执行参数、费率与杠杆。</p>
            {sources.filter((source) => source.access_level === 'configure').map((source) => (
              <div key={source.source_id} className="rounded-xl border border-border-soft p-4">
                <p className="text-sm font-medium text-ink">{source.account} <span className="font-mono text-[10px] text-subtle">{source.source_id}</span></p>
                <div className="mt-3 space-y-2">
                  {grantableUsers.map((target) => {
                    const level = (accountGrants[source.source_id] ?? []).find((grant) => grant.user_id === target.user_id)?.access_level ?? 'none'
                    return (
                      <div key={target.user_id} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border-soft px-3 py-2">
                        <span className="text-xs text-ink">{target.username}</span>
                        <LevelPicker
                          value={level}
                          disabled={savingAccount === source.source_id}
                          onChange={(next) => void saveAccountGrantLevel(source.source_id, target, next)}
                        />
                      </div>
                    )
                  })}
                  {!grantableUsers.length && <p className="text-xs text-muted">暂无可授权的普通用户</p>}
                </div>
              </div>
            ))}
            {!sources.some((source) => source.access_level === 'configure') && (
              <p className="text-sm text-muted">你暂无可管理授权的账户</p>
            )}
          </CardContent>
        </Card>
      )}
      <Card className="mt-5">
        <CardHeader><CardTitle>策略权限</CardTitle></CardHeader>
        <CardContent className="space-y-5">
          <p className="text-xs text-muted">
            策略授权独立于账户：可见用户能在目录和已绑定账户中看到该策略；可配置用户能用它新建账户绑定并管理这里的授权。新建策略默认私有；管理员可勾选全员可见或设置推送 token、推送用户。机器推送方发送 X-CTA-Publish-Token；未设置 token 的策略保持开放推送（兼容旧方式）。
          </p>
          {access.map((item) => (
            <div key={item.strategy_name} className="rounded-xl border border-border-soft p-4">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div>
                  <p className="font-mono text-sm font-medium text-ink">{item.strategy_name}</p>
                  <p className="text-xs text-muted">创建者 {item.created_by ?? '—'} · {item.publish_token_set ? '已设置推送 token' : '开放推送（未设 token）'} · {item.open_visibility ? '全员可见' : (item.grants.length ? '部分用户可见' : '私有')}</p>
                </div>
              </div>
              {isAdmin && (
                <div className="mt-3 flex flex-wrap items-center gap-2">
                  <input
                    type="text"
                    placeholder={item.publish_token_set ? '输入新 token 覆盖（8-128 字符）' : '设置 token 后开始鉴权（8-128 字符）'}
                    value={tokenDrafts[item.strategy_name] ?? ''}
                    disabled={savingStrategy === item.strategy_name}
                    onChange={(event) => setTokenDrafts((current) => ({ ...current, [item.strategy_name]: event.target.value }))}
                    className="h-8 w-72 rounded-lg border border-border-soft bg-canvas px-3 font-mono text-xs text-ink"
                  />
                  <Button size="sm" disabled={savingStrategy === item.strategy_name || !(tokenDrafts[item.strategy_name] ?? '').trim()} onClick={() => void saveToken(item)}>设置 token</Button>
                  <Button size="sm" disabled={savingStrategy === item.strategy_name} onClick={() => void resetToken(item)}>重置为随机 token</Button>
                  {item.publish_token_set && <Button size="sm" variant="danger" disabled={savingStrategy === item.strategy_name} onClick={() => void clearToken(item)}>清除 token</Button>}
                </div>
              )}
              {!!revealedTokens[item.strategy_name] && (
                <div className="mt-2 flex flex-wrap items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2">
                  <span className="text-xs text-amber-800">新 token（仅显示一次，请立即复制）：</span>
                  <code className="font-mono text-xs text-ink select-all">{revealedTokens[item.strategy_name]}</code>
                </div>
              )}
              {grantableUsers.length > 0 && (
                <div className="mt-3 space-y-3">
                  <div>
                    <div className="mb-1.5 flex items-center gap-3">
                      <p className="text-xs font-medium text-muted">用户授权（可见 / 可配置）</p>
                      {isAdmin && (
                        <label className="flex cursor-pointer items-center gap-1.5 text-xs text-muted"><input type="checkbox" checked={item.open_visibility} disabled={savingStrategy === item.strategy_name} onChange={(event) => void toggleOpen(item, event.target.checked)} /><span>全员可见</span></label>
                      )}
                    </div>
                    <div className="space-y-2">
                      {grantableUsers.map((target) => {
                        const level = item.grants.find((grant) => grant.user_id === target.user_id)?.access_level ?? 'none'
                        return (
                          <div key={target.user_id} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border-soft px-3 py-2">
                            <span className="text-xs text-ink">{target.username}</span>
                            <LevelPicker
                              value={level}
                              disabled={savingStrategy === item.strategy_name}
                              onChange={(next) => void setUserGrantLevel(item, target, next)}
                            />
                          </div>
                        )
                      })}
                    </div>
                  </div>
                  {isAdmin && (
                    <div>
                      <p className="mb-1.5 text-xs font-medium text-muted">可推送用户</p>
                      <div className="flex flex-wrap gap-2">
                        {grantableUsers.map((target) => {
                          const checked = item.managers.some((manager) => manager.user_id === target.user_id)
                          return <label key={target.user_id} className="flex cursor-pointer items-center gap-2 rounded-lg border border-border-soft px-3 py-2 text-xs text-muted hover:border-brand-ring"><input type="checkbox" checked={checked} disabled={savingStrategy === item.strategy_name} onChange={() => void toggleManager(item, target, !checked)} /><span>{target.username}</span></label>
                        })}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          ))}
          {!access.length && <p className="text-sm text-muted">{isAdmin ? '暂无仓位策略' : '你暂无可管理授权的策略'}</p>}
        </CardContent>
      </Card>
      {isAdmin && (
        <Card className="mt-5">
          <CardHeader><CardTitle>全局兜底推送 token</CardTitle></CardHeader>
          <CardContent className="space-y-4">
            <p className="text-xs text-muted">兜底 token 对所有仓位策略生效，适用于一个推送方需要推送多个策略的场景。仅存储 SHA-256 哈希；留空 token 输入框则自动生成随机 token。</p>
            {revealedFallback && (
              <div className="flex flex-wrap items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2">
                <span className="text-xs text-amber-800">新 token（仅显示一次，请立即复制）：</span>
                <code className="font-mono text-xs text-ink select-all">{revealedFallback}</code>
              </div>
            )}
            <div className="flex flex-wrap items-center gap-2">
              <input
                type="text"
                placeholder="备注（如推送方名称，可选）"
                value={fallbackNote}
                disabled={savingFallback}
                onChange={(event) => setFallbackNote(event.target.value)}
                className="h-8 w-48 rounded-lg border border-border-soft bg-canvas px-3 text-xs text-ink"
              />
              <input
                type="text"
                placeholder="自定义 token（留空自动生成）"
                value={fallbackDraft}
                disabled={savingFallback}
                onChange={(event) => setFallbackDraft(event.target.value)}
                className="h-8 w-56 rounded-lg border border-border-soft bg-canvas px-3 font-mono text-xs text-ink"
              />
              <Button size="sm" disabled={savingFallback} onClick={() => void addFallback()}>添加兜底 token</Button>
            </div>
            {fallbackTokens.map((item) => (
              <div key={item.token_id} className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border-soft px-4 py-3">
                <div>
                  <p className="text-sm font-medium text-ink">{item.note || '（无备注）'}</p>
                  <p className="text-xs text-muted">#{item.token_id} · 创建于 {item.created_at}</p>
                </div>
                <Button size="sm" variant="danger" disabled={savingFallback} onClick={() => void removeFallback(item)}>删除</Button>
              </div>
            ))}
            {!fallbackTokens.length && <p className="text-sm text-muted">暂无兜底 token</p>}
          </CardContent>
        </Card>
      )}
      <a className="mt-5 inline-flex text-sm font-medium text-brand hover:text-brand-hover" href={routes.workspace}>返回总览</a>
    </AppShell>
  )
}
