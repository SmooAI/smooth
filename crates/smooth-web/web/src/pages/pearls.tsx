import { useEffect, useMemo, useState } from 'react';
import { ChevronDown, ChevronRight, ExternalLink, Search } from 'lucide-react';
import { api } from '../api';
import { useProject } from '../context';
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card';
import { Badge } from '../components/ui/badge';
import { Tabs, TabsList, TabsTrigger } from '../components/ui/tabs';
import { cn } from '../lib/utils';

interface Pearl {
    id: string;
    title: string;
    status: string;
    priority: number;
    description?: string;
    created_at?: string;
    updated_at?: string;
    closed_at?: string;
    labels?: string[];
    pearl_type?: string;
    parent_id?: string;
    assigned_to?: string;
}

type ViewTab = 'kanban' | 'timeline' | 'stats';

const COLUMNS = [
    { key: 'open', label: 'Open', variant: 'muted' as const },
    { key: 'in_progress', label: 'In Progress', variant: 'warning' as const },
    { key: 'closed', label: 'Closed', variant: 'success' as const },
];

function extractJiraKey(title: string): string | null {
    const match = title.match(/SMOODEV-\d+/);
    return match ? match[0] : null;
}

function prioritySortKey(priority: number): number {
    return priority;
}

function sortByPriority(pearls: Pearl[]): Pearl[] {
    return [...pearls].sort((a, b) => prioritySortKey(a.priority) - prioritySortKey(b.priority));
}

function priorityLabel(priority: number): string {
    if (priority <= 1) return 'P1';
    if (priority <= 2) return 'P2';
    return `P${priority}`;
}

function PriorityBadge({ priority }: { priority: number }) {
    const variant = priority <= 1 ? 'destructive' : priority <= 2 ? 'warning' : 'muted';
    return <Badge variant={variant}>{priorityLabel(priority)}</Badge>;
}

function StatusDot({ status }: { status: string }) {
    const color =
        status === 'closed'
            ? 'bg-green-400'
            : status === 'in_progress'
              ? 'bg-yellow-400'
              : 'bg-gray-400';
    return <span className={cn('inline-block h-2.5 w-2.5 rounded-full shrink-0', color)} />;
}

function JiraLink({ jiraKey }: { jiraKey: string }) {
    return (
        <a
            href={`https://smooai.atlassian.net/browse/${jiraKey}`}
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-xs text-primary hover:underline"
        >
            <ExternalLink size={12} />
            {jiraKey}
        </a>
    );
}

// --- Kanban View ---

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
                {jiraKey && <JiraLink jiraKey={jiraKey} />}
            </CardContent>
        </Card>
    );
}

function KanbanView({ pearls }: { pearls: Pearl[] }) {
    const [closedExpanded, setClosedExpanded] = useState(false);

    const grouped = COLUMNS.map((col) => ({
        ...col,
        pearls: sortByPriority(pearls.filter((p) => p.status === col.key)),
    }));

    return (
        <div className="overflow-x-auto -mx-6 px-6 pb-4">
            <div className="flex gap-4 min-w-max md:min-w-0 md:grid md:grid-cols-3 md:gap-6">
                {grouped.map((col) => {
                    const isClosed = col.key === 'closed';
                    const isCollapsed = isClosed && !closedExpanded;

                    return (
                        <div key={col.key} className="w-72 shrink-0 md:w-auto">
                            <div className="flex items-center gap-2 mb-4 sticky top-0 bg-background py-1">
                                {isClosed && (
                                    <button
                                        onClick={() => setClosedExpanded(!closedExpanded)}
                                        className="text-muted-foreground hover:text-foreground transition-colors"
                                    >
                                        {closedExpanded ? (
                                            <ChevronDown size={16} />
                                        ) : (
                                            <ChevronRight size={16} />
                                        )}
                                    </button>
                                )}
                                <Badge variant={col.variant}>{col.label}</Badge>
                                <Badge variant="outline" className="text-xs px-1.5 py-0">
                                    {col.pearls.length}
                                </Badge>
                            </div>
                            {isCollapsed ? (
                                <button
                                    onClick={() => setClosedExpanded(true)}
                                    className="text-xs text-muted-foreground italic hover:text-foreground transition-colors cursor-pointer"
                                >
                                    {col.pearls.length} closed pearl{col.pearls.length !== 1 ? 's' : ''} — click to expand
                                </button>
                            ) : (
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
                            )}
                        </div>
                    );
                })}
            </div>
        </div>
    );
}

// --- Timeline View ---

function groupByDate(pearls: Pearl[]): { date: string; pearls: Pearl[] }[] {
    const groups: Record<string, Pearl[]> = {};
    for (const pearl of pearls) {
        const dateStr = pearl.created_at
            ? new Date(pearl.created_at).toLocaleDateString('en-US', {
                  year: 'numeric',
                  month: 'long',
                  day: 'numeric',
              })
            : 'Unknown date';
        if (!groups[dateStr]) groups[dateStr] = [];
        groups[dateStr].push(pearl);
    }

    // Sort groups by date (most recent first)
    return Object.entries(groups)
        .sort(([a], [b]) => {
            if (a === 'Unknown date') return 1;
            if (b === 'Unknown date') return -1;
            return new Date(b).getTime() - new Date(a).getTime();
        })
        .map(([date, pearls]) => ({ date, pearls: sortByPriority(pearls) }));
}

function TimelineRow({ pearl }: { pearl: Pearl }) {
    const jiraKey = extractJiraKey(pearl.title);

    return (
        <div className="flex items-center gap-3 py-2 px-3 rounded-lg hover:bg-muted/50 transition-colors">
            <StatusDot status={pearl.status} />
            <span className="font-mono text-xs text-muted-foreground w-20 shrink-0 truncate">
                {pearl.id}
            </span>
            <span className="text-sm flex-1 truncate">{pearl.title}</span>
            {jiraKey && <JiraLink jiraKey={jiraKey} />}
            <PriorityBadge priority={pearl.priority} />
        </div>
    );
}

function TimelineView({ pearls }: { pearls: Pearl[] }) {
    const groups = useMemo(() => groupByDate(pearls), [pearls]);

    if (pearls.length === 0) {
        return <p className="text-muted-foreground">No pearls to display.</p>;
    }

    return (
        <div className="space-y-6">
            {groups.map((group) => (
                <div key={group.date}>
                    <h3 className="text-sm font-semibold text-muted-foreground mb-2 sticky top-0 bg-background py-1">
                        {group.date}
                    </h3>
                    <div className="space-y-0.5">
                        {group.pearls.map((pearl) => (
                            <TimelineRow key={pearl.id} pearl={pearl} />
                        ))}
                    </div>
                </div>
            ))}
        </div>
    );
}

// --- Stats View ---

function StatCard({ label, value, subtext }: { label: string; value: string | number; subtext?: string }) {
    return (
        <Card>
            <CardContent className="p-4">
                <p className="text-xs text-muted-foreground uppercase tracking-wide">{label}</p>
                <p className="text-2xl font-bold mt-1">{value}</p>
                {subtext && <p className="text-xs text-muted-foreground mt-1">{subtext}</p>}
            </CardContent>
        </Card>
    );
}

function BarSegment({ label, count, total, color }: { label: string; count: number; total: number; color: string }) {
    const pct = total > 0 ? (count / total) * 100 : 0;
    return (
        <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground w-24 shrink-0">{label}</span>
            <div className="flex-1 h-6 bg-muted rounded-full overflow-hidden">
                <div
                    className={cn('h-full rounded-full transition-all duration-500', color)}
                    style={{ width: `${pct}%` }}
                />
            </div>
            <span className="text-xs font-mono w-16 text-right">
                {count} ({pct.toFixed(0)}%)
            </span>
        </div>
    );
}

function StatsView({ pearls }: { pearls: Pearl[] }) {
    const total = pearls.length;
    const open = pearls.filter((p) => p.status === 'open').length;
    const inProgress = pearls.filter((p) => p.status === 'in_progress').length;
    const closed = pearls.filter((p) => p.status === 'closed').length;
    const completionRate = total > 0 ? ((closed / total) * 100).toFixed(1) : '0';

    const p1 = pearls.filter((p) => p.priority <= 1).length;
    const p2 = pearls.filter((p) => p.priority === 2).length;
    const p3plus = pearls.filter((p) => p.priority >= 3).length;

    if (total === 0) {
        return <p className="text-muted-foreground">No pearls to display.</p>;
    }

    return (
        <div className="space-y-8">
            {/* Summary cards */}
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
                <StatCard label="Total" value={total} />
                <StatCard label="Open" value={open} />
                <StatCard label="In Progress" value={inProgress} />
                <StatCard label="Closed" value={closed} subtext={`${completionRate}% complete`} />
            </div>

            {/* Status breakdown bar chart */}
            <Card>
                <CardHeader className="pb-2">
                    <CardTitle className="text-sm">Status Breakdown</CardTitle>
                </CardHeader>
                <CardContent className="space-y-3">
                    <BarSegment label="Open" count={open} total={total} color="bg-gray-400" />
                    <BarSegment label="In Progress" count={inProgress} total={total} color="bg-yellow-400" />
                    <BarSegment label="Closed" count={closed} total={total} color="bg-green-400" />
                </CardContent>
            </Card>

            {/* Priority breakdown */}
            <Card>
                <CardHeader className="pb-2">
                    <CardTitle className="text-sm">Priority Breakdown</CardTitle>
                </CardHeader>
                <CardContent className="space-y-3">
                    <BarSegment label="P1 (High)" count={p1} total={total} color="bg-red-400" />
                    <BarSegment label="P2 (Medium)" count={p2} total={total} color="bg-yellow-400" />
                    <BarSegment label="P3+ (Low)" count={p3plus} total={total} color="bg-blue-400" />
                </CardContent>
            </Card>
        </div>
    );
}

// --- Project Picker ---

function ProjectPicker() {
    const { projects, setSelectedProject } = useProject();

    if (projects.length === 0) {
        return (
            <div className="rounded-lg border border-border p-6">
                <h2 className="text-base font-semibold mb-2">No projects registered</h2>
                <p className="text-sm text-muted-foreground mb-3">
                    Initialize pearls in a repository to see it here:
                </p>
                <pre className="rounded bg-muted px-3 py-2 text-xs overflow-x-auto">
                    cd ~/your/repo && th pearls init
                </pre>
            </div>
        );
    }

    return (
        <div>
            <h2 className="text-base font-semibold mb-3">Choose a project</h2>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                {projects.map((p) => {
                    const total =
                        (p.pearl_counts?.open ?? 0) +
                        (p.pearl_counts?.in_progress ?? 0) +
                        (p.pearl_counts?.closed ?? 0);
                    return (
                        <button
                            key={p.path}
                            onClick={() => setSelectedProject(p.path)}
                            type="button"
                            className="text-left rounded-lg border border-border bg-card p-4 hover:bg-muted/40 hover:border-primary/40 transition-colors cursor-pointer min-h-[88px]"
                        >
                            <div className="font-medium mb-1">{p.name}</div>
                            <div className="text-xs text-muted-foreground font-mono truncate mb-2">
                                {p.path}
                            </div>
                            <div className="flex flex-wrap gap-1.5 text-xs">
                                <Badge variant="muted">{p.pearl_counts?.open ?? 0} open</Badge>
                                <Badge variant="warning">{p.pearl_counts?.in_progress ?? 0} active</Badge>
                                <Badge variant="success">{p.pearl_counts?.closed ?? 0} closed</Badge>
                                <span className="text-muted-foreground self-center">· {total} total</span>
                            </div>
                        </button>
                    );
                })}
            </div>
        </div>
    );
}

// --- Main Page ---

export function PearlsPage() {
    const { selectedProject } = useProject();
    const [pearls, setPearls] = useState<Pearl[]>([]);
    const [loading, setLoading] = useState(true);
    const [activeTab, setActiveTab] = useState<ViewTab>('kanban');
    const [search, setSearch] = useState('');

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

    const filteredPearls = useMemo(() => {
        if (!search.trim()) return pearls;
        const q = search.toLowerCase();
        return pearls.filter(
            (p) =>
                p.title.toLowerCase().includes(q) ||
                p.id.toLowerCase().includes(q)
        );
    }, [pearls, search]);

    return (
        <div>
            <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-4 mb-6">
                <h1 className="text-2xl font-bold">Pearls</h1>
                {selectedProject && !loading && pearls.length > 0 && (
                    <div className="relative w-full sm:w-72">
                        <Search
                            size={16}
                            className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground"
                        />
                        <input
                            type="text"
                            placeholder="Filter by title or ID..."
                            value={search}
                            onChange={(e) => setSearch(e.target.value)}
                            className="w-full rounded-lg border border-border bg-background px-9 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring"
                            style={{ fontSize: '16px' }}
                        />
                        {search && (
                            <span className="absolute right-3 top-1/2 -translate-y-1/2 text-xs text-muted-foreground">
                                {filteredPearls.length}/{pearls.length}
                            </span>
                        )}
                    </div>
                )}
            </div>

            {!selectedProject && <ProjectPicker />}
            {selectedProject && loading && (
                <p className="text-muted-foreground">Loading pearls...</p>
            )}
            {selectedProject && !loading && pearls.length === 0 && (
                <p className="text-muted-foreground">No pearls found for this project.</p>
            )}
            {selectedProject && !loading && pearls.length > 0 && (
                <>
                    <Tabs className="mb-6">
                        <TabsList>
                            <TabsTrigger
                                active={activeTab === 'kanban'}
                                onClick={() => setActiveTab('kanban')}
                            >
                                Kanban
                            </TabsTrigger>
                            <TabsTrigger
                                active={activeTab === 'timeline'}
                                onClick={() => setActiveTab('timeline')}
                            >
                                Timeline
                            </TabsTrigger>
                            <TabsTrigger
                                active={activeTab === 'stats'}
                                onClick={() => setActiveTab('stats')}
                            >
                                Stats
                            </TabsTrigger>
                        </TabsList>
                    </Tabs>

                    {activeTab === 'kanban' && <KanbanView pearls={filteredPearls} />}
                    {activeTab === 'timeline' && <TimelineView pearls={filteredPearls} />}
                    {activeTab === 'stats' && <StatsView pearls={filteredPearls} />}
                </>
            )}
        </div>
    );
}
