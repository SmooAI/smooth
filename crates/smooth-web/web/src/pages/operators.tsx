import { useEffect, useState } from 'react';
import { api } from '../api';

export function OperatorsPage() {
    const [ops, setOps] = useState<any[]>([]);
    useEffect(() => { api<{ data: any[] }>('/api/workers').then((r) => setOps(r.data)).catch(() => {}); }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Smooth Operators</h1>
            {ops.length === 0 && <p style={{ color: 'var(--muted)' }}>No active Smooth Operators.</p>}
            {ops.map((op, i) => (
                <div key={i} className="rounded-lg p-4 mb-2" style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}>
                    <span className="font-mono" style={{ color: 'var(--smoo-green)' }}>{op.id || op.worker_id}</span>
                </div>
            ))}
        </div>
    );
}
