'use client';

import { useEffect, useState } from 'react';

import { api } from '@/lib/api';
import { cn } from '@/lib/utils';

export default function SystemPage() {
    const [health, setHealth] = useState<any>(null);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any }>('/api/system/health')
            .then((r) => setHealth(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">System Health</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {health && (
                <div className="flex flex-col gap-3">
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
    const dotColor = status === 'healthy' || status === 'connected' ? 'bg-green-500' : status === 'degraded' ? 'bg-yellow-500' : 'bg-red-500';

    return (
        <div className="bg-neutral-900 border border-neutral-800 rounded-lg p-4 flex items-center gap-3">
            <div className={cn('w-2.5 h-2.5 rounded-full', dotColor)} />
            <div className="font-semibold w-24">{label}</div>
            <div className="text-neutral-500 text-sm">{status}</div>
            <div className="text-neutral-600 text-sm ml-auto">{detail}</div>
        </div>
    );
}
