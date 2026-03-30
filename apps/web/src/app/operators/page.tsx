'use client';

import { useEffect, useState } from 'react';
import { api } from '@/lib/api';

export default function OperatorsPage() {
    const [operators, setOperators] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any[] }>('/api/workers')
            .then((r) => setOperators(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    return (
        <div>
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>Smooth Operators</h1>
            {loading && <p style={{ color: '#737373' }}>Loading...</p>}
            {!loading && operators.length === 0 && <p style={{ color: '#737373' }}>No active Smooth Operators.</p>}
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                {operators.map((op, i) => (
                    <div key={i} style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16 }}>
                        <div style={{ display: 'flex', gap: 12, alignItems: 'center' }}>
                            <span style={{ color: '#06b6d4', fontFamily: 'monospace' }}>{op.workerId}</span>
                            <span style={{ color: '#eab308', fontSize: 12, fontWeight: 600 }}>[{op.phase}]</span>
                            <span style={{ color: '#737373', fontSize: 13 }}>bead: {op.beadId}</span>
                            <span style={{ marginLeft: 'auto', color: '#525252', fontSize: 12 }}>{op.status}</span>
                        </div>
                    </div>
                ))}
            </div>
        </div>
    );
}
