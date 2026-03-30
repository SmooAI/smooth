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
            <h1 className="text-2xl font-bold mb-6">Smooth Operators</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {!loading && operators.length === 0 && <p className="text-neutral-500">No active Smooth Operators.</p>}
            <div className="flex flex-col gap-2">
                {operators.map((op, i) => (
                    <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-4 flex items-center gap-3">
                        <span className="text-cyan-400 font-mono">{op.workerId}</span>
                        <span className="text-yellow-500 text-xs font-semibold">[{op.phase}]</span>
                        <span className="text-neutral-500 text-sm">bead: {op.beadId}</span>
                        <span className="ml-auto text-neutral-600 text-xs">{op.status}</span>
                    </div>
                ))}
            </div>
        </div>
    );
}
