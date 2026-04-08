import { useEffect, useState } from 'react';
import { ExternalLink } from 'lucide-react';
import { api } from '../api';
import { useProject } from '../context';
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card';
import { Badge } from '../components/ui/badge';

interface Pearl {
    id: string;
    title: string;
    status: string;
    priority: number;
    description?: string;
}

const COLUMNS = [
    { key: 'open', label: 'Open', variant: 'muted' as const },
    { key: 'in_progress', label: 'In Progress', variant: 'warning' as const },
    { key: 'closed', label: 'Closed', variant: 'success' as const },
];

function extractJiraKey(title: string): string | null {
    const match = title.match(/SMOODEV-\d+/);
    return match ? match[0] : null;
}

function PriorityBadge({ priority }: { priority: number }) {
    const variant = priority <= 1 ? 'destructive' : priority <= 2 ? 'warning' : 'muted';
    return <Badge variant={variant}>P{priority}</Badge>;
}

function PearlCard({ pearl }: { pearl: Pearl }) {
    const jiraKey = extractJiraKey(pearl.title);

    return (
        <Card className="mb-3">
            <CardContent className="p-4">
                <div className="flex items-start justify-between gap-2 mb-2">
                    <span className="font-mono text-xs text-primary">{pearl.id}</span>
                    <PriorityBadge priority={pearl.priority} />
                </div>
                <p className="text-sm font-medium leading-snug mb-2">{pearl.title}</p>
                {jiraKey && (
                    <a
                        href={`https://smooai.atlassian.net/browse/${jiraKey}`}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="inline-flex items-center gap-1 text-xs text-primary hover:underline"
                    >
                        <ExternalLink size={12} />
                        {jiraKey}
                    </a>
                )}
            </CardContent>
        </Card>
    );
}

export function PearlsPage() {
    const { selectedProject } = useProject();
    const [pearls, setPearls] = useState<Pearl[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        if (!selectedProject) {
            setPearls([]);
            setLoading(false);
            return;
        }
        setLoading(true);
        api<{ data: Pearl[] }>(`/api/projects/pearls?path=${encodeURIComponent(selectedProject)}`)
            .then((r) => setPearls(r.data))
            .catch(() => setPearls([]))
            .finally(() => setLoading(false));
    }, [selectedProject]);

    const grouped = COLUMNS.map((col) => ({
        ...col,
        pearls: pearls.filter((p) => p.status === col.key),
    }));

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Pearls</h1>
            {!selectedProject && (
                <p className="text-muted-foreground">Select a project to view pearls.</p>
            )}
            {selectedProject && loading && (
                <p className="text-muted-foreground">Loading pearls...</p>
            )}
            {selectedProject && !loading && pearls.length === 0 && (
                <p className="text-muted-foreground">No pearls found for this project.</p>
            )}
            {selectedProject && !loading && pearls.length > 0 && (
                <div className="grid grid-cols-3 gap-6">
                    {grouped.map((col) => (
                        <div key={col.key}>
                            <div className="flex items-center gap-2 mb-4">
                                <Badge variant={col.variant}>{col.label}</Badge>
                                <span className="text-sm text-muted-foreground">
                                    {col.pearls.length}
                                </span>
                            </div>
                            <div>
                                {col.pearls.map((pearl) => (
                                    <PearlCard key={pearl.id} pearl={pearl} />
                                ))}
                                {col.pearls.length === 0 && (
                                    <p className="text-xs text-muted-foreground italic">
                                        No {col.label.toLowerCase()} pearls
                                    </p>
                                )}
                            </div>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
}
