'use client';

import { useEffect, useState } from 'react';

import { api } from '@/lib/api';

export default function MessagesPage() {
    const [inbox, setInbox] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any[] }>('/api/messages/inbox')
            .then((r) => setInbox(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Messages</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {!loading && inbox.length === 0 && <p className="text-neutral-500">No messages requiring attention.</p>}
            <div className="flex flex-col gap-3">
                {inbox.map((item, i) => (
                    <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
                        <div className="flex items-center gap-2 mb-2">
                            <span className="text-cyan-400 font-mono text-sm">{item.message?.beadId}</span>
                            <span className="font-semibold">{item.beadTitle}</span>
                            {item.requiresAction && <span className="bg-yellow-900/50 text-yellow-400 px-2 py-0.5 rounded text-xs">{item.actionType}</span>}
                        </div>
                        <div className="text-neutral-400 text-sm">{item.message?.content}</div>
                    </div>
                ))}
            </div>
        </div>
    );
}
