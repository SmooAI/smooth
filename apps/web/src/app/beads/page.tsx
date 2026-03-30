'use client';

import { useEffect, useState } from 'react';

import { api } from '@/lib/api';
import { cn } from '@/lib/utils';

export default function BeadsPage() {
    const [beads, setBeads] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any[] }>('/api/beads')
            .then((r) => setBeads(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    const statusClass = (s: string) =>
        s === 'closed' ? 'text-green-500' : s === 'blocked' ? 'text-red-500' : s === 'in_progress' ? 'text-yellow-500' : 'text-neutral-500';

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Beads</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {!loading && beads.length === 0 && <p className="text-neutral-500">No beads found.</p>}
            <div className="flex flex-col gap-2">
                {beads.map((b, i) => (
                    <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-4 flex gap-3 items-center">
                        <span className={cn('text-xs font-semibold uppercase', statusClass(b.status))}>{b.status}</span>
                        <span className="text-cyan-400 font-mono text-sm">{b.id}</span>
                        <span className="flex-1">{b.title}</span>
                        <span className="text-neutral-600 text-xs">P{b.priority}</span>
                    </div>
                ))}
            </div>
        </div>
    );
}
