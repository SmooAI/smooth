import { useCallback, useEffect, useState } from 'react';
import { Activity, Database, Shield, ShieldCheck, Circle } from 'lucide-react';
import { api } from '../api';
import { useProject } from '../context';
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card';
import { Badge } from '../components/ui/badge';

export function DashboardPage() {
    const [health, setHealth] = useState<any>(null);
    const [error, setError] = useState<string | null>(null);
    const { projects } = useProject();

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
            {error && (
                <div className="bg-destructive/10 border border-destructive/30 rounded-lg p-4 mb-4 text-destructive-foreground">
                    {error}
                </div>
            )}
            {health && (
                <div className="grid grid-cols-2 md:grid-cols-4 gap-3 mb-8">
                    <HealthCard icon={Activity} title="Leader" status={health.leader?.status} detail={`${Math.round(health.leader?.uptime ?? 0)}s`} />
                    <HealthCard icon={Database} title="Database" status={health.database?.status} detail="SQLite" />
                    <HealthCard icon={Shield} title="Sandbox" status={health.sandbox?.status} detail={`${health.sandbox?.active_sandboxes ?? 0}/${health.sandbox?.max_concurrency ?? 0}`} />
                    <HealthCard icon={ShieldCheck} title="Tailscale" status={health.tailscale?.status === 'connected' ? 'healthy' : 'down'} detail={health.tailscale?.hostname ?? 'off'} />
                </div>
            )}

            {projects.length > 0 && (
                <>
                    <h2 className="text-lg font-semibold mb-4">Pearl Summary</h2>
                    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
                        {projects.map((project) => (
                            <Card key={project.path}>
                                <CardHeader className="pb-3">
                                    <CardTitle className="text-sm flex items-center gap-2">
                                        <Circle size={14} className="text-primary" />
                                        {project.name}
                                    </CardTitle>
                                </CardHeader>
                                <CardContent>
                                    <div className="flex gap-2">
                                        <Badge variant="muted">
                                            {project.pearl_counts.open} open
                                        </Badge>
                                        <Badge variant="warning">
                                            {project.pearl_counts.in_progress} active
                                        </Badge>
                                        <Badge variant="success">
                                            {project.pearl_counts.closed} closed
                                        </Badge>
                                    </div>
                                </CardContent>
                            </Card>
                        ))}
                    </div>
                </>
            )}
        </div>
    );
}

function HealthCard({ icon: Icon, title, status, detail }: { icon: any; title: string; status: string; detail: string }) {
    const color = status === 'healthy' ? 'bg-green-500' : status === 'degraded' ? 'bg-yellow-500' : 'bg-red-500';
    return (
        <Card>
            <CardContent className="p-4">
                <div className="flex items-center gap-2 mb-2">
                    <div className={`h-2 w-2 rounded-full shrink-0 ${color}`} />
                    <Icon size={14} className="text-muted-foreground shrink-0" />
                    <span className="font-semibold text-sm truncate">{title}</span>
                </div>
                <div className="text-xs text-muted-foreground">{detail}</div>
            </CardContent>
        </Card>
    );
}
