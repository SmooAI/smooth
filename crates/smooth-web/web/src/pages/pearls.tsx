import { useEffect, useState } from 'react';
import { api } from '../api';

export function PearlsPage() {
    const [pearls, setPearls] = useState<any[]>([]);
    useEffect(() => { api<{ data: any[] }>('/api/pearls').then((r) => setPearls(r.data)).catch(() => {}); }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Pearls</h1>
            {pearls.length === 0 && <p style={{ color: 'var(--muted)' }}>No pearls found.</p>}
            <div className="flex flex-col gap-2">
                {pearls.map((p, i) => (
                    <div key={i} className="rounded-lg p-4 flex items-center gap-3" style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}>
                        <span className="text-xs font-semibold uppercase" style={{ color: p.status === 'closed' ? '#22c55e' : p.status === 'in_progress' ? '#eab308' : 'var(--muted)' }}>{p.status}</span>
                        <span className="font-mono text-sm" style={{ color: 'var(--smoo-green)' }}>{p.id}</span>
                        <span className="flex-1">{p.title}</span>
                        <span className="text-xs" style={{ color: 'var(--muted)' }}>P{p.priority}</span>
                    </div>
                ))}
            </div>
        </div>
    );
}
