import { useCallback, useEffect, useState } from 'react';
import { Activity, Database, Shield, ShieldCheck } from 'lucide-react';
import { api } from '../api';

export function DashboardPage() {
    const [health, setHealth] = useState<any>(null);
    const [error, setError] = useState<string | null>(null);

    const load = useCallback(() => {
        api<{ data: any }>('/api/system/health').then((r) => setHealth(r.data)).catch((e) => setError(e.message));
    }, []);

    useEffect(() => {
        load();
        const i = setInterval(load, 5000);
        return () => clearInterval(i);
    }, [load]);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Dashboard</h1>
            {error && <div className="bg-red-950/50 border border-red-900 rounded-lg p-4 mb-4">{error}</div>}
            {health && (
                <div className="grid grid-cols-4 gap-4 mb-6">
                    <Card icon={Activity} title="Leader" status={health.leader?.status} detail={`${Math.round(health.leader?.uptime ?? 0)}s`} />
                    <Card icon={Database} title="Database" status={health.database?.status} detail="SQLite" />
                    <Card icon={Shield} title="Sandbox" status={health.sandbox?.status} detail={`${health.sandbox?.active_sandboxes ?? 0}/${health.sandbox?.max_concurrency ?? 0}`} />
                    <Card icon={ShieldCheck} title="Tailscale" status={health.tailscale?.status === 'connected' ? 'healthy' : 'down'} detail={health.tailscale?.hostname ?? 'off'} />
                </div>
            )}
        </div>
    );
}

function Card({ icon: Icon, title, status, detail }: { icon: any; title: string; status: string; detail: string }) {
    const dot = status === 'healthy' ? 'bg-green-500' : status === 'degraded' ? 'bg-yellow-500' : 'bg-red-500';
    return (
        <div className="rounded-lg p-4" style={{ background: 'var(--smoo-dark-blue-850)', border: '1px solid var(--border)' }}>
            <div className="flex items-center gap-2 mb-2">
                <div className={`w-2 h-2 rounded-full ${dot}`} />
                <Icon size={14} style={{ color: 'var(--muted)' }} />
                <span className="font-semibold text-sm">{title}</span>
            </div>
            <div className="text-xs" style={{ color: 'var(--muted)' }}>{detail}</div>
        </div>
    );
}
