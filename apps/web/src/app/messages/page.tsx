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
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>Messages</h1>
            {loading && <p style={{ color: '#737373' }}>Loading...</p>}
            {!loading && inbox.length === 0 && <p style={{ color: '#737373' }}>No messages requiring attention.</p>}
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                {inbox.map((item, i) => (
                    <div key={i} style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16 }}>
                        <div style={{ display: 'flex', gap: 8, marginBottom: 8, alignItems: 'center' }}>
                            <span style={{ color: '#06b6d4', fontFamily: 'monospace', fontSize: 13 }}>{item.message?.beadId}</span>
                            <span style={{ fontWeight: 600 }}>{item.beadTitle}</span>
                            {item.requiresAction && (
                                <span style={{ background: '#422006', color: '#fbbf24', padding: '2px 8px', borderRadius: 4, fontSize: 11 }}>
                                    {item.actionType}
                                </span>
                            )}
                        </div>
                        <div style={{ color: '#a3a3a3', fontSize: 14 }}>{item.message?.content}</div>
                    </div>
                ))}
            </div>
        </div>
    );
}
