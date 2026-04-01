import { useEffect, useState } from 'react';
import { api } from '../api';

export function BeadsPage() {
    const [beads, setBeads] = useState<any[]>([]);
    useEffect(() => { api<{ data: any[] }>('/api/beads').then((r) => setBeads(r.data)).catch(() => {}); }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Beads</h1>
            {beads.length === 0 && <p style={{ color: 'var(--muted)' }}>No beads found.</p>}
            <div className="flex flex-col gap-2">
                {beads.map((b, i) => (
                    <div key={i} className="rounded-lg p-4 flex items-center gap-3" style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}>
                        <span className="text-xs font-semibold uppercase" style={{ color: b.status === 'closed' ? '#22c55e' : b.status === 'in_progress' ? '#eab308' : 'var(--muted)' }}>{b.status}</span>
                        <span className="font-mono text-sm" style={{ color: 'var(--smoo-green)' }}>{b.id}</span>
                        <span className="flex-1">{b.title}</span>
                        <span className="text-xs" style={{ color: 'var(--muted)' }}>P{b.priority}</span>
                    </div>
                ))}
            </div>
        </div>
    );
}
