'use client';

import { useEffect, useState } from 'react';
import { api, apiPost } from '@/lib/api';

export default function ReviewsPage() {
    const [reviews, setReviews] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    const load = () => {
        api<{ data: any[] }>('/api/reviews')
            .then((r) => setReviews(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    };

    useEffect(load, []);

    const approve = async (beadId: string) => {
        await apiPost(`/api/reviews/${beadId}/approve`, {});
        load();
    };

    const reject = async (beadId: string) => {
        const reason = prompt('Rejection reason:');
        if (!reason) return;
        await apiPost(`/api/reviews/${beadId}/reject`, { reason });
        load();
    };

    return (
        <div>
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>Reviews</h1>
            {loading && <p style={{ color: '#737373' }}>Loading...</p>}
            {!loading && reviews.length === 0 && <p style={{ color: '#737373' }}>No pending reviews.</p>}
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                {reviews.map((r, i) => (
                    <div key={i} style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16 }}>
                        <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 12 }}>
                            <span style={{ color: '#06b6d4', fontFamily: 'monospace', fontSize: 13 }}>{r.id}</span>
                            <span>{r.title}</span>
                            <span style={{ background: '#422006', color: '#fbbf24', padding: '2px 8px', borderRadius: 4, fontSize: 11 }}>pending</span>
                        </div>
                        <div style={{ display: 'flex', gap: 8 }}>
                            <button
                                onClick={() => approve(r.id)}
                                style={{ background: '#166534', color: '#fff', border: 'none', borderRadius: 6, padding: '6px 16px', cursor: 'pointer' }}
                            >
                                Approve
                            </button>
                            <button
                                onClick={() => reject(r.id)}
                                style={{ background: '#991b1b', color: '#fff', border: 'none', borderRadius: 6, padding: '6px 16px', cursor: 'pointer' }}
                            >
                                Reject
                            </button>
                        </div>
                    </div>
                ))}
            </div>
        </div>
    );
}
