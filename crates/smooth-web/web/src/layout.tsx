import { Link, Outlet, useLocation } from 'react-router-dom';
import { LayoutDashboard, Circle, Bot, MessageSquare, Settings } from 'lucide-react';

const NAV = [
    { path: '/', label: 'Dashboard', icon: LayoutDashboard },
    { path: '/pearls', label: 'Pearls', icon: Circle },
    { path: '/operators', label: 'Operators', icon: Bot },
    { path: '/chat', label: 'Chat', icon: MessageSquare },
    { path: '/system', label: 'System', icon: Settings },
];

export function Layout() {
    const location = useLocation();

    return (
        <div className="flex min-h-screen" style={{ background: 'var(--smoo-dark-blue)' }}>
            <nav className="w-52 p-4 flex flex-col gap-1" style={{ borderRight: '1px solid var(--border)' }}>
                <div className="mb-4 px-3">
                    <img src="/logo.svg" alt="Smoo AI" className="h-8" />
                </div>
                <div className="text-xs font-semibold uppercase tracking-wider px-3 mb-2" style={{ color: 'var(--muted)' }}>
                    Smooth
                </div>
                {NAV.map(({ path, label, icon: Icon }) => {
                    const active = location.pathname === path;
                    return (
                        <Link
                            key={path}
                            to={path}
                            className="flex items-center gap-3 px-3 py-2 rounded-md text-sm transition-colors"
                            style={{ color: active ? 'var(--smoo-green)' : 'var(--muted)', fontWeight: active ? 600 : 400 }}
                        >
                            <Icon size={16} />
                            {label}
                        </Link>
                    );
                })}
            </nav>
            <main className="flex-1 p-6">
                <Outlet />
            </main>
        </div>
    );
}
