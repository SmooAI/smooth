'use client';

import { Pause, Play, Send, X } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { api, apiPost } from '@/lib/api';

export default function OperatorsPage() {
    const [operators, setOperators] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);
    const [steerBeadId, setSteerBeadId] = useState<string | null>(null);
    const [steerMsg, setSteerMsg] = useState('');

    const load = useCallback(() => {
        api<{ data: any[] }>('/api/workers')
            .then((r) => setOperators(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    useEffect(() => {
        load();
        const interval = setInterval(load, 5000);
        return () => clearInterval(interval);
    }, [load]);

    const pause = async (beadId: string) => {
        await apiPost(`/api/steering/${beadId}/pause`, {});
        load();
    };
    const resume = async (beadId: string) => {
        await apiPost(`/api/steering/${beadId}/resume`, {});
        load();
    };
    const cancel = async (beadId: string) => {
        await apiPost(`/api/steering/${beadId}/cancel`, {});
        load();
    };
    const steer = async (beadId: string, message: string) => {
        await apiPost(`/api/steering/${beadId}/steer`, { message });
        setSteerBeadId(null);
        setSteerMsg('');
    };

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Smooth Operators</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {!loading && operators.length === 0 && <p className="text-neutral-500">No active Smooth Operators.</p>}
            <div className="flex flex-col gap-3">
                {operators.map((op, i) => {
                    const phaseColor =
                        op.phase === 'execute' ? 'text-yellow-500' : op.phase === 'finalize' ? 'text-green-500' : op.phase === 'review' ? 'text-purple-400' : 'text-neutral-400';

                    return (
                        <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
                            <div className="flex items-center gap-3 mb-3">
                                <span className="text-cyan-400 font-mono">{op.workerId}</span>
                                <span className={`text-xs font-semibold ${phaseColor}`}>[{op.phase}]</span>
                                <span className="text-neutral-500 text-sm">bead: {op.beadId}</span>
                                <span className="ml-auto text-neutral-600 text-xs">{op.status}</span>
                            </div>

                            {/* Steering controls */}
                            <div className="flex items-center gap-2">
                                <button onClick={() => pause(op.beadId)} className="flex items-center gap-1 bg-yellow-900/40 hover:bg-yellow-900/60 text-yellow-400 text-xs rounded px-3 py-1.5 cursor-pointer transition-colors" title="Pause operator">
                                    <Pause size={12} /> Pause
                                </button>
                                <button onClick={() => resume(op.beadId)} className="flex items-center gap-1 bg-green-900/40 hover:bg-green-900/60 text-green-400 text-xs rounded px-3 py-1.5 cursor-pointer transition-colors" title="Resume operator">
                                    <Play size={12} /> Resume
                                </button>
                                <button onClick={() => setSteerBeadId(steerBeadId === op.beadId ? null : op.beadId)} className="flex items-center gap-1 bg-blue-900/40 hover:bg-blue-900/60 text-blue-400 text-xs rounded px-3 py-1.5 cursor-pointer transition-colors" title="Send guidance">
                                    <Send size={12} /> Steer
                                </button>
                                <button onClick={() => cancel(op.beadId)} className="flex items-center gap-1 bg-red-900/40 hover:bg-red-900/60 text-red-400 text-xs rounded px-3 py-1.5 cursor-pointer transition-colors ml-auto" title="Cancel operator">
                                    <X size={12} /> Cancel
                                </button>
                            </div>

                            {/* Steer input */}
                            {steerBeadId === op.beadId && (
                                <div className="flex gap-2 mt-3">
                                    <input
                                        value={steerMsg}
                                        onChange={(e) => setSteerMsg(e.target.value)}
                                        onKeyDown={(e) => e.key === 'Enter' && steerMsg.trim() && steer(op.beadId, steerMsg)}
                                        placeholder="Type guidance for the operator..."
                                        className="flex-1 bg-neutral-800 border border-neutral-700 rounded px-3 py-2 text-sm text-neutral-100 outline-none focus:border-blue-600"
                                        autoFocus
                                    />
                                    <button
                                        onClick={() => steerMsg.trim() && steer(op.beadId, steerMsg)}
                                        disabled={!steerMsg.trim()}
                                        className="bg-blue-600 hover:bg-blue-500 disabled:opacity-50 text-white text-sm rounded px-4 py-2 cursor-pointer transition-colors"
                                    >
                                        Send
                                    </button>
                                </div>
                            )}
                        </div>
                    );
                })}
            </div>
        </div>
    );
}
