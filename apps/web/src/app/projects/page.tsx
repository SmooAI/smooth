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
            <h1 style={{ fontSize: 24, fontWeight: 700, marginBottom: 24 }}>Projects</h1>
            {loading && <p style={{ color: '#737373' }}>Loading...</p>}
            {!loading && projects.length === 0 && <p style={{ color: '#737373' }}>No projects. Create one: <code>th project create &lt;name&gt;</code></p>}
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                {projects.map((p, i) => (
                    <div key={i} style={{ background: '#171717', border: '1px solid #262626', borderRadius: 8, padding: 16 }}>
                        <div style={{ fontWeight: 600 }}>{p.title ?? p.name}</div>
                        <div style={{ color: '#737373', fontSize: 13 }}>{p.id} &middot; {p.status ?? 'open'}</div>
                    </div>
                ))}
            </div>
        </div>
    );
}
