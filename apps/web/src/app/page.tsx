'use client';

import { useEffect, useState } from 'react';
import { Activity, Database, Shield } from 'lucide-react';
import { api } from '@/lib/api';
import { cn } from '@/lib/utils';

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
            <h1 className="text-2xl font-bold mb-6">Dashboard</h1>

            {error && (
                <div className="bg-red-950/50 border border-red-900 rounded-lg p-4 mb-4">
                    <strong>Cannot reach leader:</strong> {error}
                </div>
            )}

            {health && (
                <div className="grid grid-cols-3 gap-4">
                    <StatusCard icon={Activity} title="Leader" status="healthy" detail={`Uptime: ${Math.round(health.uptime)}s`} />
                    <StatusCard icon={Database} title="Database" status="healthy" detail="SQLite (~/.smooth/smooth.db)" />
                    <StatusCard icon={Shield} title="Sandbox" status="healthy" detail="Microsandbox (local)" />
                </div>
            )}

            {!health && !error && <p className="text-neutral-500">Loading...</p>}
        </div>
    );
}

function StatusCard({ icon: Icon, title, status, detail }: { icon: React.ComponentType<{ size?: number; className?: string }>; title: string; status: string; detail: string }) {
    const dotColor = status === 'healthy' ? 'bg-green-500' : status === 'degraded' ? 'bg-yellow-500' : 'bg-red-500';

    return (
        <div className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
            <div className="flex items-center gap-2 mb-2">
                <div className={cn('w-2 h-2 rounded-full', dotColor)} />
                <Icon size={14} className="text-neutral-500" />
                <span className="font-semibold">{title}</span>
            </div>
            <div className="text-neutral-500 text-sm">{detail}</div>
        </div>
    );
}
