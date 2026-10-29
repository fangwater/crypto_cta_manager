import { PencilLine } from 'lucide-react'
import { useState } from 'react'
import { saveAccountAlias } from '../api'
import type { DashboardAccount } from '../types'
import { Button } from './ui/Button'
import { Input } from './ui/Field'

export function AccountAliasEditor({ account, onSaved }: {
  account: DashboardAccount
  onSaved: (update: { source_id: string; account: string; display_alias: string | null }) => void
}) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  function startEditing() {
    if (!account.can_edit_alias) return
    setDraft(account.display_alias ?? account.account)
    setError(null)
    setEditing(true)
  }

  async function save() {
    if (saving) return
    setSaving(true)
    setError(null)
    try {
      onSaved(await saveAccountAlias(account.source_id, draft.trim() || null))
      setEditing(false)
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSaving(false)
    }
  }

  if (!editing || !account.can_edit_alias) return (
    <span className="inline-flex max-w-full items-center gap-1.5">
      <span className="break-all" onDoubleClick={startEditing} title={account.can_edit_alias ? '双击修改别名' : undefined}>{account.account}</span>
      {account.can_edit_alias && <button type="button" onClick={startEditing} aria-label={`修改 ${account.account} 的别名`} title="修改别名" className="shrink-0 rounded p-1 text-muted hover:bg-canvas hover:text-brand"><PencilLine size={13} /></button>}
    </span>
  )

  return (
    <span className="grid min-w-0 gap-2">
      <Input autoFocus aria-label="账户别名" value={draft} maxLength={200} disabled={saving}
        placeholder="留空恢复配置名称" onChange={event => setDraft(event.target.value)}
        onKeyDown={event => {
          if (event.nativeEvent.isComposing) return
          if (event.key === 'Enter') { event.preventDefault(); void save() }
          if (event.key === 'Escape' && !saving) { event.preventDefault(); setEditing(false) }
        }} />
      <span className="flex flex-wrap items-center gap-2">
        <Button size="sm" disabled={saving} onClick={() => void save()}>{saving ? '保存中…' : '保存'}</Button>
        <Button size="sm" variant="ghost" disabled={saving} onClick={() => setEditing(false)}>取消</Button>
      </span>
      <span className="text-xs font-normal text-muted">留空恢复配置名称</span>
      {error && <span role="alert" className="text-xs font-normal text-rose-700">{error}</span>}
    </span>
  )
}
