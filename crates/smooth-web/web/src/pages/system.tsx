import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';

// --- Types ---

interface HealthData {
    leader?: { status: string; uptime: number };
    database?: { status: string; path: string };
    sandbox?: { status: string; backend: string; active_sandboxes: number; max_concurrency: number };
    tailscale?: { status: string; hostname: string };
    pearls?: { status: string; open_pearls: number };
}

interface Worker {
    id: string;
    name?: string;
    status?: string;
}

// --- Constants ---

const SMOO_GREEN = '#3ad67d';
const COLOR_HEALTHY = '#22c55e';
const COLOR_DEGRADED = '#eab308';
const COLOR_DOWN = '#ef4444';
const COLOR_INACTIVE = '#6b7280';
const NODE_LABEL_COLOR = '#e5e7eb';
const NODE_SUBLABEL_COLOR = '#9ca3af';
const LINE_COLOR = 'rgba(255,255,255,0.12)';

const CENTER = { x: 400, y: 300 };
const INNER_RADIUS = 140;
const OUTER_RADIUS = 260;

// --- Helpers ---

function statusColor(status: string | undefined): string {
    if (!status) return COLOR_INACTIVE;
    const s = status.toLowerCase();
    if (s === 'healthy' || s === 'connected' || s === 'ok') return COLOR_HEALTHY;
    if (s === 'degraded') return COLOR_DEGRADED;
    if (s === 'not_connected' || s === 'inactive') return COLOR_INACTIVE;
    return COLOR_DOWN;
}

function statusLabel(status: string | undefined): string {
    if (!status) return 'Unknown';
    return status.charAt(0).toUpperCase() + status.slice(1).replace(/_/g, ' ');
}

function formatUptime(seconds: number): string {
    if (seconds < 60) return `${Math.round(seconds)}s`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    return `${h}h ${m}m`;
}

function polarToXY(cx: number, cy: number, radius: number, angleDeg: number): { x: number; y: number } {
    const rad = (angleDeg - 90) * (Math.PI / 180);
    return { x: cx + radius * Math.cos(rad), y: cy + radius * Math.sin(rad) };
}

// --- Node definitions ---

interface TopoNode {
    id: string;
    label: string;
    sublabel: string;
    color: string;
    x: number;
    y: number;
    radius: number;
    parentId?: string;
    pulse: boolean;
}

function buildNodes(health: HealthData | null, workers: Worker[]): TopoNode[] {
    const nodes: TopoNode[] = [];

    // Center: Big Smooth
    const leaderColor = health?.leader ? statusColor(health.leader.status) : COLOR_INACTIVE;
    nodes.push({
        id: 'leader',
        label: 'Big Smooth',
        sublabel: health?.leader ? formatUptime(health.leader.uptime) : 'offline',
        color: health?.leader?.status === 'healthy' ? SMOO_GREEN : leaderColor,
        x: CENTER.x,
        y: CENTER.y,
        radius: 32,
        pulse: health?.leader?.status === 'healthy',
    });

    // Inner ring: infrastructure services
    const innerNodes = [
        {
            id: 'database',
            label: 'Dolt store',
            sublabel: 'pearls + config',
            color: statusColor(health?.database?.status),
            pulse: health?.database?.status === 'healthy',
        },
        {
            id: 'sandbox',
            label: 'Smooth Operators',
            sublabel: `${health?.sandbox?.active_sandboxes ?? 0}/${health?.sandbox?.max_concurrency ?? 3} active`,
            color: statusColor(health?.sandbox?.status),
            pulse: health?.sandbox?.status === 'healthy',
        },
        {
            id: 'tailscale',
            label: 'Tailscale',
            sublabel: health?.tailscale?.hostname ?? 'disconnected',
            color: statusColor(health?.tailscale?.status),
            pulse: health?.tailscale?.status === 'connected',
        },
        {
            id: 'pearls',
            label: 'Pearls',
            sublabel: `${health?.pearls?.open_pearls ?? 0} open`,
            color: statusColor(health?.pearls?.status),
            pulse: health?.pearls?.status === 'healthy',
        },
    ];

    const innerAngleStep = 360 / innerNodes.length;
    innerNodes.forEach((n, i) => {
        const pos = polarToXY(CENTER.x, CENTER.y, INNER_RADIUS, i * innerAngleStep);
        nodes.push({ ...n, x: pos.x, y: pos.y, radius: 20, parentId: 'leader' });
    });

    // Outer ring: security cast
    const outerNodes = [
        { id: 'wonk', label: 'Wonk', sublabel: 'Access Control' },
        { id: 'goalie', label: 'Goalie', sublabel: 'Network Proxy' },
        { id: 'narc', label: 'Narc', sublabel: 'Tool Surveillance' },
        { id: 'scribe', label: 'Scribe', sublabel: 'Logging' },
        { id: 'archivist', label: 'Archivist', sublabel: 'Log Aggregator' },
    ];

    const outerAngleStep = 360 / outerNodes.length;
    outerNodes.forEach((n, i) => {
        const pos = polarToXY(CENTER.x, CENTER.y, OUTER_RADIUS, i * outerAngleStep + 36);
        nodes.push({
            ...n,
            x: pos.x,
            y: pos.y,
            radius: 16,
            color: COLOR_INACTIVE,
            parentId: 'leader',
            pulse: false,
        });
    });

    // Dynamic: operators from sandbox pool
    if (workers.length > 0) {
        const sandboxNode = nodes.find((n) => n.id === 'sandbox');
        if (sandboxNode) {
            const workerAngleBase = 90; // below center
            const workerSpread = 30;
            const startAngle = workerAngleBase - ((workers.length - 1) * workerSpread) / 2;
            workers.forEach((w, i) => {
                const angle = startAngle + i * workerSpread;
                const pos = polarToXY(sandboxNode.x, sandboxNode.y, 70, angle);
                nodes.push({
                    id: `worker-${w.id}`,
                    label: w.name ?? `Operator ${w.id}`,
                    sublabel: w.status ?? 'running',
                    color: COLOR_HEALTHY,
                    x: pos.x,
                    y: pos.y,
                    radius: 14,
                    parentId: 'sandbox',
                    pulse: true,
                });
            });
        }
    }

    return nodes;
}

// --- SVG Components ---

function ConnectionLine({ from, to }: { from: TopoNode; to: TopoNode }) {
    return <line x1={from.x} y1={from.y} x2={to.x} y2={to.y} stroke={LINE_COLOR} strokeWidth={1.5} />;
}

function TopoNodeSVG({
    node,
    onHover,
    onLeave,
}: {
    node: TopoNode;
    onHover: (node: TopoNode, e: React.MouseEvent) => void;
    onLeave: () => void;
}) {
    const isCenter = node.id === 'leader';
    return (
        <g
            onMouseEnter={(e) => onHover(node, e)}
            onMouseLeave={onLeave}
            style={{ cursor: 'pointer' }}
        >
            {/* Pulse ring for active nodes */}
            {node.pulse && (
                <circle cx={node.x} cy={node.y} r={node.radius + 4} fill="none" stroke={node.color} strokeWidth={1.5} opacity={0.4}>
                    <animate attributeName="r" from={node.radius + 2} to={node.radius + 10} dur="2s" repeatCount="indefinite" />
                    <animate attributeName="opacity" from={0.5} to={0} dur="2s" repeatCount="indefinite" />
                </circle>
            )}
            {/* Node circle */}
            <circle
                cx={node.x}
                cy={node.y}
                r={node.radius}
                fill={isCenter ? node.color : `${node.color}22`}
                stroke={node.color}
                strokeWidth={isCenter ? 3 : 2}
            />
            {/* Inner glow for center */}
            {isCenter && (
                <circle cx={node.x} cy={node.y} r={node.radius - 6} fill="none" stroke="rgba(255,255,255,0.15)" strokeWidth={1} />
            )}
            {/* Label */}
            <text
                x={node.x}
                y={node.y + node.radius + 16}
                textAnchor="middle"
                fill={NODE_LABEL_COLOR}
                fontSize={isCenter ? 13 : 11}
                fontWeight={isCenter ? 600 : 500}
            >
                {node.label}
            </text>
            {/* Sublabel */}
            <text
                x={node.x}
                y={node.y + node.radius + 30}
                textAnchor="middle"
                fill={NODE_SUBLABEL_COLOR}
                fontSize={10}
            >
                {node.sublabel}
            </text>
        </g>
    );
}

// --- Tooltip ---

function Tooltip({ node, x, y }: { node: TopoNode; x: number; y: number }) {
    return (
        <div
            className="fixed z-50 pointer-events-none rounded-lg px-3 py-2 text-sm shadow-lg border"
            style={{
                left: x + 12,
                top: y - 10,
                background: 'oklch(0.17 0.02 260)',
                borderColor: 'oklch(0.3 0.02 260)',
                color: NODE_LABEL_COLOR,
                maxWidth: 220,
            }}
        >
            <div className="font-semibold">{node.label}</div>
            <div className="text-xs" style={{ color: NODE_SUBLABEL_COLOR }}>
                {node.sublabel}
            </div>
            <div className="flex items-center gap-1.5 mt-1">
                <span className="inline-block w-2 h-2 rounded-full" style={{ background: node.color }} />
                <span className="text-xs">{node.pulse ? 'Active' : 'Inactive'}</span>
            </div>
        </div>
    );
}

// --- Details Table ---

function DetailsTable({ health }: { health: HealthData }) {
    const rows = [
        {
            label: 'Big Smooth',
            status: health.leader?.status,
            detail: health.leader ? `Uptime: ${formatUptime(health.leader.uptime)}` : '--',
        },
        {
            label: 'Dolt store',
            status: health.database?.status,
            detail: health.database?.path ?? '--',
        },
        {
            label: 'Smooth Operators',
            status: health.sandbox?.status,
            detail: health.sandbox ? `${health.sandbox.backend} (${health.sandbox.active_sandboxes}/${health.sandbox.max_concurrency} active)` : '--',
        },
        {
            label: 'Tailscale',
            status: health.tailscale?.status,
            detail: health.tailscale?.hostname ?? 'disconnected',
        },
        {
            label: 'Pearls',
            status: health.pearls?.status,
            detail: `${health.pearls?.open_pearls ?? 0} open`,
        },
    ];

    return (
        <div className="mt-8">
            <h2 className="text-lg font-semibold mb-3" style={{ color: NODE_LABEL_COLOR }}>
                Details
            </h2>
            <div className="rounded-lg overflow-hidden border" style={{ borderColor: 'oklch(0.3 0.02 260)' }}>
                <table className="w-full text-sm">
                    <thead>
                        <tr style={{ background: 'oklch(0.15 0.015 260)' }}>
                            <th className="text-left px-4 py-2.5 font-semibold" style={{ color: NODE_SUBLABEL_COLOR }}>Service</th>
                            <th className="text-left px-4 py-2.5 font-semibold" style={{ color: NODE_SUBLABEL_COLOR }}>Status</th>
                            <th className="text-left px-4 py-2.5 font-semibold" style={{ color: NODE_SUBLABEL_COLOR }}>Details</th>
                        </tr>
                    </thead>
                    <tbody>
                        {rows.map((row) => (
                            <tr key={row.label} style={{ borderTop: '1px solid oklch(0.25 0.02 260)' }}>
                                <td className="px-4 py-2.5 font-medium" style={{ color: NODE_LABEL_COLOR }}>{row.label}</td>
                                <td className="px-4 py-2.5">
                                    <span className="inline-flex items-center gap-1.5">
                                        <span
                                            className="inline-block w-2 h-2 rounded-full"
                                            style={{ background: statusColor(row.status) }}
                                        />
                                        <span style={{ color: NODE_SUBLABEL_COLOR }}>{statusLabel(row.status)}</span>
                                    </span>
                                </td>
                                <td className="px-4 py-2.5" style={{ color: '#6b7280' }}>{row.detail}</td>
                            </tr>
                        ))}
                    </tbody>
                </table>
            </div>
        </div>
    );
}

// --- Main Page ---

export function SystemPage() {
    const [health, setHealth] = useState<HealthData | null>(null);
    const [workers, setWorkers] = useState<Worker[]>([]);
    const [tooltip, setTooltip] = useState<{ node: TopoNode; x: number; y: number } | null>(null);
    const prevHealthRef = useRef<string>('');

    const fetchData = useCallback(() => {
        api<{ data: HealthData }>('/api/system/health')
            .then((r) => setHealth(r.data))
            .catch(() => {});
        api<{ data: Worker[] }>('/api/workers')
            .then((r) => setWorkers(Array.isArray(r.data) ? r.data : []))
            .catch(() => setWorkers([]));
    }, []);

    useEffect(() => {
        fetchData();
        const interval = setInterval(fetchData, 5000);
        return () => clearInterval(interval);
    }, [fetchData]);

    // Track status changes for transition animation
    const healthKey = health ? JSON.stringify(health) : '';
    const hasChanged = prevHealthRef.current !== '' && prevHealthRef.current !== healthKey;
    useEffect(() => {
        prevHealthRef.current = healthKey;
    }, [healthKey]);

    const nodes = buildNodes(health, workers);
    const nodeMap = new Map(nodes.map((n) => [n.id, n]));

    const handleHover = useCallback((node: TopoNode, e: React.MouseEvent) => {
        setTooltip({ node, x: e.clientX, y: e.clientY });
    }, []);

    const handleLeave = useCallback(() => {
        setTooltip(null);
    }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">System Topology</h1>

            {/* SVG Topology Graph */}
            <div
                className="rounded-xl border overflow-hidden"
                style={{
                    background: 'oklch(0.1 0.015 260)',
                    borderColor: 'oklch(0.25 0.02 260)',
                }}
            >
                <svg
                    viewBox="0 0 800 600"
                    className="w-full"
                    style={{ maxHeight: '70vh' }}
                >
                    {/* Transition flash */}
                    {hasChanged && (
                        <rect x={0} y={0} width={800} height={600} fill="white" opacity={0}>
                            <animate attributeName="opacity" from={0.04} to={0} dur="0.6s" fill="freeze" />
                        </rect>
                    )}

                    {/* Radial ring guides */}
                    <circle cx={CENTER.x} cy={CENTER.y} r={INNER_RADIUS} fill="none" stroke="rgba(255,255,255,0.04)" strokeWidth={1} strokeDasharray="4 6" />
                    <circle cx={CENTER.x} cy={CENTER.y} r={OUTER_RADIUS} fill="none" stroke="rgba(255,255,255,0.03)" strokeWidth={1} strokeDasharray="4 6" />

                    {/* Connection lines */}
                    {nodes
                        .filter((n) => n.parentId)
                        .map((n) => {
                            const parent = nodeMap.get(n.parentId!);
                            if (!parent) return null;
                            return <ConnectionLine key={`line-${n.id}`} from={parent} to={n} />;
                        })}

                    {/* Nodes (center last for z-order) */}
                    {nodes
                        .filter((n) => n.id !== 'leader')
                        .map((n) => (
                            <TopoNodeSVG key={n.id} node={n} onHover={handleHover} onLeave={handleLeave} />
                        ))}
                    {nodes
                        .filter((n) => n.id === 'leader')
                        .map((n) => (
                            <TopoNodeSVG key={n.id} node={n} onHover={handleHover} onLeave={handleLeave} />
                        ))}
                </svg>
            </div>

            {/* Tooltip overlay */}
            {tooltip && <Tooltip node={tooltip.node} x={tooltip.x} y={tooltip.y} />}

            {/* Details table below */}
            {health && <DetailsTable health={health} />}
        </div>
    );
}
