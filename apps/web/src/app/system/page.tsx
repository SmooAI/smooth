'use client';

import { useEffect, useState } from 'react';
import { api } from '@/lib/api';

export default function SystemPage() {
    const [health, setHealth] = useState<any>(null);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any }>('/api/system/health')
            .then((r) => setHealth(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    const statusColor = (s: string) => (s === 'healthy' || s === 'connected' ? '#22c55e' : s === 'degraded' ? '#eab308' : '#ef4444');
    const statusDot = (s: string) => <span style={{ display: 'inline-block', width: 8, height: 8, borderRadius: '50%', background: statusColor(s), marginRight: 8 }} />;

    return (
        <div>
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>System Health</h1>
            {loading && <p style={{ color: '#737373' }}>Loading...</p>}
            {health && (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                    <HealthRow label="Leader" status={health.leader?.status} detail={`Uptime: ${Math.round(health.leader?.uptime ?? 0)}s`} />
                    <HealthRow label="Database" status={health.database?.status} detail={health.database?.path ?? 'unknown'} />
                    <HealthRow
                        label="Sandbox"
                        status={health.sandbox?.status}
                        detail={`${health.sandbox?.backend ?? 'unknown'} (${health.sandbox?.activeSandboxes ?? 0}/${health.sandbox?.maxConcurrency ?? 0})`}
                    />
                    <HealthRow label="Tailscale" status={health.tailscale?.status} detail={health.tailscale?.hostname ?? 'not connected'} />
                    <HealthRow label="Beads" status={health.beads?.status} detail={`${health.beads?.openIssues ?? 0} open issues`} />
                </div>
            )}
        </div>
    );
}

function HealthRow({ label, status, detail }: { label: string; status: string; detail: string }) {
    const color = status === 'healthy' || status === 'connected' ? '#22c55e' : status === 'degraded' ? '#eab308' : '#ef4444';
    return (
        <div style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16, display: 'flex', alignItems: 'center', gap: 12 }}>
            <div style={{ width: 10, height: 10, borderRadius: '50%', background: color }} />
            <div style={{ fontWeight: 600, width: 100 }}>{label}</div>
            <div style={{ color: '#737373', fontSize: 13 }}>{status}</div>
            <div style={{ color: '#525252', fontSize: 13, marginLeft: 'auto' }}>{detail}</div>
        </div>
    );
}
