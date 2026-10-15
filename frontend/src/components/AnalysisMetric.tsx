export function AnalysisMetric({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return <div className="summary-item"><div><span>{label}</span><strong>{value}</strong>{detail && <small>{detail}</small>}</div></div>
}
