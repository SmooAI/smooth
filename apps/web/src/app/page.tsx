'use client';

import { useEffect, useState } from 'react';
import { api } from '@/lib/api';

interface Health {
    ok: boolean;
    service: string;
    uptime: number;
}

export default function DashboardPage() {
    const [health, setHealth] = useState<Health | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        api<Health>('/health')
            .then(setHealth)
            .catch((e) => setError(e.message));
    }, []);

    return (
        <div>
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>Dashboard</h1>

            {error && (
                <div style={{ background: '#451a1a', border: '1px solid #991b1b', borderRadius: 8, padding: 16, marginBottom: 16 }}>
                    <strong>Cannot reach leader:</strong> {error}
                </div>
            )}

            {health && (
                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 16 }}>
                    <StatusCard title="Leader" status="healthy" detail={`Uptime: ${Math.round(health.uptime)}s`} />
                    <StatusCard title="Database" status="healthy" detail="SQLite (~/.smooth/smooth.db)" />
                    <StatusCard title="Sandbox" status="healthy" detail="Microsandbox (local)" />
                </div>
            )}

            {!health && !error && <p style={{ color: '#737373' }}>Loading...</p>}
        </div>
    );
}

function StatusCard({ title, status, detail }: { title: string; status: string; detail: string }) {
    const color = status === 'healthy' ? '#22c55e' : status === 'degraded' ? '#eab308' : '#ef4444';
    return (
        <div style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
                <div style={{ width: 8, height: 8, borderRadius: '50%', background: color }} />
                <span style={{ fontWeight: 600 }}>{title}</span>
            </div>
            <div style={{ color: '#a3a3a3', fontSize: 13 }}>{detail}</div>
        </div>
    );
}
