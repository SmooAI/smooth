'use client';

import { Activity, Bot, Database, MessageSquare, Shield, ShieldCheck } from 'lucide-react';
import Link from 'next/link';
import { useCallback, useEffect, useState } from 'react';

import { api } from '@/lib/api';
import { useWebSocket } from '@/lib/use-websocket';
import { cn } from '@/lib/utils';

export default function DashboardPage() {
    const [health, setHealth] = useState<any>(null);
    const [operators, setOperators] = useState<any[]>([]);
    const [reviews, setReviews] = useState<any[]>([]);
    const [inbox, setInbox] = useState<any[]>([]);
    const [error, setError] = useState<string | null>(null);

    const load = useCallback(() => {
        api<{ data: any }>('/api/system/health').then((r) => setHealth(r.data)).catch((e) => setError(e.message));
        api<{ data: any[] }>('/api/workers').then((r) => setOperators(r.data)).catch(() => {});
        api<{ data: any[] }>('/api/reviews').then((r) => setReviews(r.data)).catch(() => {});
        api<{ data: any[] }>('/api/messages/inbox').then((r) => setInbox(r.data)).catch(() => {});
    }, []);

    // WebSocket for real-time events — triggers reload on relevant events
    const { status: wsStatus } = useWebSocket({
        topics: ['*'],
        onEvent: () => load(),
    });

    useEffect(() => {
        load();
        // Fallback polling in case WebSocket is down
        const interval = setInterval(load, wsStatus === 'connected' ? 30_000 : 5_000);
        return () => clearInterval(interval);
    }, [load, wsStatus]);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Dashboard</h1>

            {error && (
                <div className="bg-red-950/50 border border-red-900 rounded-lg p-4 mb-4">
                    <strong>Cannot reach leader:</strong> {error}
                </div>
            )}

            {/* Health cards */}
            {health && (
                <div className="grid grid-cols-4 gap-4 mb-6">
                    <StatusCard icon={Activity} title="Leader" status={health.leader?.status} detail={`${Math.round(health.leader?.uptime ?? 0)}s`} />
                    <StatusCard icon={Database} title="Database" status={health.database?.status} detail="SQLite" />
                    <StatusCard
                        icon={Shield}
                        title="Sandbox"
                        status={health.sandbox?.status}
                        detail={`${health.sandbox?.activeSandboxes ?? 0}/${health.sandbox?.maxConcurrency ?? 0}`}
                    />
                    <StatusCard icon={ShieldCheck} title="Tailscale" status={health.tailscale?.status === 'connected' ? 'healthy' : 'down'} detail={health.tailscale?.hostname ?? 'disconnected'} />
                </div>
            )}

            {/* Activity summary */}
            <div className="grid grid-cols-3 gap-4 mb-6">
                <MetricCard icon={Bot} label="Active Operators" value={operators.length} color="cyan" />
                <MetricCard icon={ShieldCheck} label="Pending Reviews" value={reviews.length} color="yellow" />
                <MetricCard icon={MessageSquare} label="Messages" value={inbox.length} color="blue" />
            </div>

            {/* Active operators */}
            {operators.length > 0 && (
                <div className="mb-6">
                    <h2 className="text-lg font-semibold mb-3">Active Operators</h2>
                    <div className="flex flex-col gap-2">
                        {operators.slice(0, 8).map((op, i) => {
                            const phaseColor =
                                op.phase === 'execute' ? 'text-yellow-500' : op.phase === 'finalize' ? 'text-green-500' : op.phase === 'review' ? 'text-purple-400' : 'text-neutral-400';
                            return (
                                <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-3 flex items-center gap-3">
                                    <span className="text-cyan-400 font-mono text-sm">{op.workerId}</span>
                                    <span className={`text-xs font-semibold ${phaseColor}`}>[{op.phase}]</span>
                                    <span className="text-neutral-500 text-sm">{op.beadId}</span>
                                    <Link href="/operators" className="ml-auto text-neutral-600 text-xs hover:text-neutral-400">
                                        manage
                                    </Link>
                                </div>
                            );
                        })}
                    </div>
                </div>
            )}

            {/* Pending reviews */}
            {reviews.length > 0 && (
                <div>
                    <h2 className="text-lg font-semibold mb-3">Pending Reviews</h2>
                    <div className="flex flex-col gap-2">
                        {reviews.slice(0, 5).map((r, i) => (
                            <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-3 flex items-center gap-3">
                                <span className="text-cyan-400 font-mono text-sm">{r.id}</span>
                                <span>{r.title}</span>
                                <Link href="/reviews" className="ml-auto text-yellow-500 text-xs hover:text-yellow-400">
                                    review
                                </Link>
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {!health && !error && <p className="text-neutral-500">Loading...</p>}
            <div className="flex items-center gap-2 mt-6">
                <div className={cn('w-2 h-2 rounded-full', wsStatus === 'connected' ? 'bg-green-500' : wsStatus === 'reconnecting' ? 'bg-yellow-500' : 'bg-red-500')} />
                <span className="text-neutral-700 text-xs">
                    {wsStatus === 'connected' ? 'Live (WebSocket)' : wsStatus === 'reconnecting' ? 'Reconnecting...' : 'Polling (5s)'}
                </span>
            </div>
        </div>
    );
}

function StatusCard({
    icon: Icon,
    title,
    status,
    detail,
}: {
    icon: React.ComponentType<{ size?: number; className?: string }>;
    title: string;
    status: string;
    detail: string;
}) {
    const dotColor = status === 'healthy' ? 'bg-green-500' : status === 'degraded' ? 'bg-yellow-500' : 'bg-red-500';
    return (
        <div className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
            <div className="flex items-center gap-2 mb-2">
                <div className={cn('w-2 h-2 rounded-full', dotColor)} />
                <Icon size={14} className="text-neutral-500" />
                <span className="font-semibold text-sm">{title}</span>
            </div>
            <div className="text-neutral-500 text-xs">{detail}</div>
        </div>
    );
}

function MetricCard({ icon: Icon, label, value, color }: { icon: React.ComponentType<{ size?: number; className?: string }>; label: string; value: number; color: string }) {
    const colorClass = color === 'cyan' ? 'text-cyan-400' : color === 'yellow' ? 'text-yellow-400' : 'text-blue-400';
    return (
        <div className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
            <div className="flex items-center gap-2 mb-1">
                <Icon size={14} className="text-neutral-500" />
                <span className="text-neutral-500 text-xs">{label}</span>
            </div>
            <span className={`text-2xl font-bold ${colorClass}`}>{value}</span>
        </div>
    );
}
