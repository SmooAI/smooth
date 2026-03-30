'use client';

import { useEffect, useState } from 'react';
import { api } from '@/lib/api';

export default function BeadsPage() {
    const [beads, setBeads] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any[] }>('/api/beads')
            .then((r) => setBeads(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    const statusColor = (s: string) => (s === 'closed' ? '#22c55e' : s === 'blocked' ? '#ef4444' : s === 'in_progress' ? '#eab308' : '#737373');

    return (
        <div>
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>Beads</h1>
            {loading && <p style={{ color: '#737373' }}>Loading...</p>}
            {!loading && beads.length === 0 && <p style={{ color: '#737373' }}>No beads found.</p>}
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                {beads.map((b, i) => (
                    <div key={i} style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16, display: 'flex', gap: 12, alignItems: 'center' }}>
                        <span style={{ color: statusColor(b.status), fontSize: 12, fontWeight: 600, textTransform: 'uppercase' }}>{b.status}</span>
                        <span style={{ color: '#06b6d4', fontFamily: 'monospace', fontSize: 13 }}>{b.id}</span>
                        <span style={{ flex: 1 }}>{b.title}</span>
                        <span style={{ color: '#525252', fontSize: 12 }}>P{b.priority}</span>
                    </div>
                ))}
            </div>
        </div>
    );
}
