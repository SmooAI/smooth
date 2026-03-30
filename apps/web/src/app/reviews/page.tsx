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
            <h1 className="text-2xl font-bold mb-6">Reviews</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {!loading && reviews.length === 0 && <p className="text-neutral-500">No pending reviews.</p>}
            <div className="flex flex-col gap-3">
                {reviews.map((r, i) => (
                    <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
                        <div className="flex items-center gap-2 mb-3">
                            <span className="text-cyan-400 font-mono text-sm">{r.id}</span>
                            <span>{r.title}</span>
                            <span className="bg-yellow-900/50 text-yellow-400 px-2 py-0.5 rounded text-xs">pending</span>
                        </div>
                        <div className="flex gap-2">
                            <button
                                onClick={() => approve(r.id)}
                                className="bg-green-800 hover:bg-green-700 text-white text-sm rounded-md px-4 py-1.5 cursor-pointer transition-colors"
                            >
                                Approve
                            </button>
                            <button
                                onClick={() => reject(r.id)}
                                className="bg-red-800 hover:bg-red-700 text-white text-sm rounded-md px-4 py-1.5 cursor-pointer transition-colors"
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
