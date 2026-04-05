import { useEffect, useState } from 'react';
import { api } from '../api';

export function SystemPage() {
    const [health, setHealth] = useState<any>(null);
    useEffect(() => { api<{ data: any }>('/api/system/health').then((r) => setHealth(r.data)).catch(() => {}); }, []);

    const dot = (s: string) => s === 'healthy' || s === 'connected' ? 'bg-green-500' : s === 'degraded' ? 'bg-yellow-500' : 'bg-red-500';

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">System Health</h1>
            {health && (
                <div className="flex flex-col gap-3">
                    {[
                        { label: 'Leader', status: health.leader?.status, detail: `Uptime: ${Math.round(health.leader?.uptime ?? 0)}s` },
                        { label: 'Database', status: health.database?.status, detail: health.database?.path },
                        { label: 'Sandbox', status: health.sandbox?.status, detail: `${health.sandbox?.backend} (${health.sandbox?.active_sandboxes}/${health.sandbox?.max_concurrency})` },
                        { label: 'Tailscale', status: health.tailscale?.status, detail: health.tailscale?.hostname ?? 'disconnected' },
                        { label: 'Pearls', status: health.pearls?.status, detail: `${health.pearls?.open_pearls ?? 0} open` },
                    ].map((row) => (
                        <div key={row.label} className="rounded-lg p-4 flex items-center gap-3" style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}>
                            <div className={`w-2.5 h-2.5 rounded-full ${dot(row.status)}`} />
                            <div className="font-semibold w-24">{row.label}</div>
                            <div className="text-sm" style={{ color: 'var(--muted)' }}>{row.status}</div>
                            <div className="text-sm ml-auto" style={{ color: '#525252' }}>{row.detail}</div>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
}
