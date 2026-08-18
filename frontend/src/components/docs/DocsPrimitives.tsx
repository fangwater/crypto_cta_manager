import { useState, type ReactNode } from 'react'
import { Check, Copy } from 'lucide-react'
import { cn } from '../../lib/cn'

export type HttpMethod = 'GET' | 'POST' | 'PUT' | 'DELETE'

const methodTone: Record<HttpMethod, string> = {
  GET: 'text-sky-700',
  POST: 'text-emerald-700',
  PUT: 'text-amber-700',
  DELETE: 'text-rose-700',
}

export function MethodBadge({ method }: { method: HttpMethod }) {
  return (
    <span
      className={cn(
        'inline-block w-14 shrink-0 font-mono text-[11px] font-semibold tracking-wide',
        methodTone[method],
      )}
    >
      {method}
    </span>
  )
}

export function CodeBlock({ children, label }: { children: string; label?: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="not-prose my-4 overflow-hidden rounded-lg border border-border bg-[#0b1220]">
      <div className="flex items-center justify-between border-b border-white/10 px-3 py-1.5">
        <span className="font-mono text-[11px] text-white/45">{label ?? 'shell'}</span>
        <button
          type="button"
          className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-white/55 hover:bg-white/10 hover:text-white"
          onClick={async () => {
            await navigator.clipboard.writeText(children)
            setCopied(true)
            window.setTimeout(() => setCopied(false), 1200)
          }}
        >
          {copied ? <Check size={12} /> : <Copy size={12} />}
          {copied ? '已复制' : '复制'}
        </button>
      </div>
      <pre className="overflow-x-auto p-3 font-mono text-[12px] leading-6 text-[#e8eef7]">
        <code>{children}</code>
      </pre>
    </div>
  )
}

export function Note({ children, tone = 'note' }: { children: ReactNode; tone?: 'note' | 'warn' }) {
  return (
    <div
      className={cn(
        'not-prose my-4 border-l-2 py-1 pl-3 text-[13px] leading-6',
        tone === 'warn'
          ? 'border-amber-500 text-amber-950'
          : 'border-brand text-ink/80',
      )}
    >
      {children}
    </div>
  )
}

export function Endpoint({
  method,
  path,
  summary,
}: {
  method: HttpMethod
  path: string
  summary?: string
}) {
  return (
    <div className="not-prose my-4 rounded-lg border border-border bg-surface px-3 py-2.5">
      <div className="flex items-start gap-2">
        <MethodBadge method={method} />
        <code className="min-w-0 flex-1 break-all font-mono text-[12.5px] text-ink">{path}</code>
      </div>
      {summary ? <p className="mt-1.5 pl-16 text-[12px] leading-5 text-muted">{summary}</p> : null}
    </div>
  )
}

export function ApiTable({
  rows,
}: {
  rows: Array<{ method: HttpMethod; path: string; summary: string }>
}) {
  return (
    <div className="not-prose my-4 overflow-hidden rounded-lg border border-border">
      {rows.map((row) => (
        <div
          key={`${row.method}-${row.path}`}
          className="grid gap-1 border-b border-border-soft px-3 py-2.5 last:border-b-0 sm:grid-cols-[4.5rem_minmax(0,1.4fr)_minmax(0,1fr)] sm:items-baseline sm:gap-3"
        >
          <MethodBadge method={row.method} />
          <code className="break-all font-mono text-[12px] text-ink">{row.path}</code>
          <span className="text-[12px] leading-5 text-muted">{row.summary}</span>
        </div>
      ))}
    </div>
  )
}

export function SpecTable({
  headers,
  rows,
}: {
  headers: string[]
  rows: Array<Array<ReactNode>>
}) {
  return (
    <div className="not-prose my-4 overflow-x-auto rounded-lg border border-border">
      <table className="w-full min-w-[28rem] text-left text-[13px]">
        <thead className="bg-canvas text-[11px] uppercase tracking-[0.08em] text-muted">
          <tr>
            {headers.map((header) => (
              <th key={header} className="px-3 py-2 font-medium">
                {header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={index} className="border-t border-border-soft">
              {row.map((cell, cellIndex) => (
                <td key={cellIndex} className="px-3 py-2.5 align-top leading-6 text-ink">
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
