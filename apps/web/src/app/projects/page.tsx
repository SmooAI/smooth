'use client';

import { useEffect, useState } from 'react';

import { api } from '@/lib/api';

export default function ProjectsPage() {
    const [projects, setProjects] = useState<any[]>([]);
    const [loading, setLoading] = useState(true);

    useEffect(() => {
        api<{ data: any[] }>('/api/projects')
            .then((r) => setProjects(r.data))
            .catch(() => {})
            .finally(() => setLoading(false));
    }, []);

    return (
        <div>
            <h1 className="text-2xl font-bold mb-6">Projects</h1>
            {loading && <p className="text-neutral-500">Loading...</p>}
            {!loading && projects.length === 0 && (
                <p className="text-neutral-500">
                    No projects. Create one: <code className="bg-neutral-800 px-2 py-0.5 rounded text-sm">th project create &lt;name&gt;</code>
                </p>
            )}
            <div className="flex flex-col gap-2">
                {projects.map((p, i) => (
                    <div key={i} className="bg-neutral-900 border border-neutral-800 rounded-lg p-4">
                        <div className="font-semibold">{p.title ?? p.name}</div>
                        <div className="text-neutral-500 text-sm">
                            {p.id} &middot; {p.status ?? 'open'}
                        </div>
                    </div>
                ))}
            </div>
        </div>
    );
}
